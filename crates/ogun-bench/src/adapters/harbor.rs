//! Harbor task directories, the format Terminal-Bench 2.0 is distributed in (O-06 section 9.1; unverified).
//!
//! ```text
//! <task>/
//!   instruction.md          the instruction
//!   task.toml               version, [metadata], [agent] timeout_sec, [verifier] timeout_sec,
//!                           [environment] build_timeout_sec, docker_image, cpus, memory or memory_mb
//!   environment/Dockerfile  the task image (docker-compose environments are not supported yet)
//!   tests/test.sh           the verifier; writes /logs/verifier/reward.txt (or reward.json)
//!   solution/solve.sh       the reference solution (never shown to the agent, not hashed into the task)
//! ```
//!
//! The verifier runs unmodified: `/tests` is staged after the agent finished and `bash /tests/test.sh` runs in
//! the same container. Harbor's `test.sh` files commonly install their test runner from the network, so the
//! verifier phase needs `evaluator_network: allowed` in the matrix; without it such a task scores as an evaluator
//! failure, which the results show.

use std::path::Path;

use crate::BenchResult;
use crate::adapters::{Part, digest_parts, invalid, memory_mb, read_text, secs_to_ms};
use crate::evaluator::{EvaluatorSpec, FULL_SCORE_PPM, ResultSource};
use crate::task::{Task, TaskEnvironment};

const ADAPTER: &str = "harbor";
/// Default agent limit when `task.toml` names none.
pub const DEFAULT_AGENT_TIMEOUT_MS: u64 = 1_800_000;
/// Default verifier limit.
pub const DEFAULT_VERIFIER_TIMEOUT_MS: u64 = 600_000;

/// Loads one task directory, or every task directory directly below `root`.
///
/// # Errors
///
/// `bench.task_invalid` or `bench.task_unsupported` for the first bad task.
pub fn load(root: &Path) -> BenchResult<Vec<Task>> {
    if root.join("task.toml").is_file() {
        return Ok(vec![load_task(root)?]);
    }
    let mut out = vec![];
    let rd = std::fs::read_dir(root)
        .map_err(|_| invalid(ADAPTER, root.display().to_string(), "not a directory"))?;
    for e in rd.flatten() {
        if e.path().join("task.toml").is_file() {
            out.push(load_task(&e.path())?);
        }
    }
    if out.is_empty() {
        return Err(invalid(
            ADAPTER,
            root.display().to_string(),
            "no task.toml in it or in its subdirectories",
        ));
    }
    Ok(out)
}

fn num(t: &toml::Table, section: &str, key: &str) -> Option<f64> {
    let v = t.get(section)?.as_table()?.get(key)?;
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

fn text<'a>(t: &'a toml::Table, section: &str, key: &str) -> Option<&'a str> {
    t.get(section)?.as_table()?.get(key)?.as_str()
}

/// Loads one Harbor task.
///
/// # Errors
///
/// `bench.task_invalid` when `instruction.md`, `task.toml` or `tests/test.sh` is missing or unreadable;
/// `bench.task_unsupported` for a docker-compose environment without a Dockerfile or image.
pub fn load_task(dir: &Path) -> BenchResult<Task> {
    let id = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            invalid(
                ADAPTER,
                dir.display().to_string(),
                "the directory name is not UTF-8",
            )
        })?
        .to_owned();
    let instruction = read_text(ADAPTER, &id, &dir.join("instruction.md"))?;
    let toml_text = read_text(ADAPTER, &id, &dir.join("task.toml"))?;
    let t: toml::Table = toml_text
        .parse()
        .map_err(|_| invalid(ADAPTER, &id, "task.toml is not valid TOML"))?;
    let tests = dir.join("tests");
    if !tests.join("test.sh").is_file() {
        return Err(invalid(ADAPTER, &id, "tests/test.sh is missing"));
    }
    let env_dir = dir.join("environment");
    let dockerfile = env_dir.join("Dockerfile");
    let image = text(&t, "environment", "docker_image").map(str::to_owned);
    let build_context = dockerfile.is_file().then(|| env_dir.clone());
    if build_context.is_none() && image.is_none() {
        let compose = ["docker-compose.yaml", "docker-compose.yml", "compose.yaml"]
            .iter()
            .any(|f| env_dir.join(f).is_file());
        return Err(crate::BenchError::TaskUnsupported {
            task: id,
            reason: if compose {
                "docker-compose environments are not supported yet".into()
            } else {
                "environment/Dockerfile is missing and task.toml names no docker_image".into()
            },
        });
    }
    let memory = num(&t, "environment", "memory_mb")
        .map(|m| m as u64)
        .or_else(|| text(&t, "environment", "memory").and_then(memory_mb));
    let version = t.get("version").and_then(|v| v.as_str()).map(str::to_owned);
    let task_blake3 = digest_parts(&[
        ("instruction.md", Part::Text(&instruction)),
        ("task.toml", Part::Text(&toml_text)),
        (
            "environment",
            if env_dir.is_dir() {
                Part::Tree(&env_dir)
            } else {
                Part::Absent
            },
        ),
        ("tests", Part::Tree(&tests)),
    ])?;
    Ok(Task {
        benchmark: "terminal_bench".into(),
        id,
        version: version.clone(),
        dir: dir.to_path_buf(),
        instruction,
        environment: TaskEnvironment {
            image,
            build_context,
            workdir: "/app".into(),
            workdir_from_image: true,
            wants_network: false,
            cpus: num(&t, "environment", "cpus").map(|c| c.max(1.0) as u32),
            memory_mb: memory,
            build_timeout_ms: secs_to_ms(num(&t, "environment", "build_timeout_sec")),
        },
        fixtures: vec![],
        benchmark_files: None,
        eval_files: Some(tests),
        setup_script: None,
        evaluator: EvaluatorSpec {
            name: "harbor.verifier".into(),
            version: version.unwrap_or_else(|| "unversioned".into()),
            command: vec!["bash".into(), "/tests/test.sh".into()],
            timeout_ms: secs_to_ms(num(&t, "verifier", "timeout_sec"))
                .unwrap_or(DEFAULT_VERIFIER_TIMEOUT_MS),
            result: ResultSource::RewardFile {
                paths: vec!["verifier/reward.txt".into(), "verifier/reward.json".into()],
            },
            pass_threshold_ppm: FULL_SCORE_PPM,
        },
        agent_timeout_ms: secs_to_ms(num(&t, "agent", "timeout_sec"))
            .unwrap_or(DEFAULT_AGENT_TIMEOUT_MS),
        task_blake3,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimal_task_directory_loads() {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().join("hello-world");
        std::fs::create_dir_all(t.join("environment")).unwrap();
        std::fs::create_dir_all(t.join("tests")).unwrap();
        std::fs::create_dir_all(t.join("solution")).unwrap();
        std::fs::write(
            t.join("instruction.md"),
            "Create hello.txt containing Hello, world!",
        )
        .unwrap();
        std::fs::write(
            t.join("task.toml"),
            "version = \"1.0\"\n[agent]\ntimeout_sec = 120.0\n[verifier]\ntimeout_sec = 60\n[environment]\nmemory = \"2G\"\ncpus = 1\n",
        )
        .unwrap();
        std::fs::write(
            t.join("environment").join("Dockerfile"),
            "FROM ubuntu:24.04\nWORKDIR /app\n",
        )
        .unwrap();
        std::fs::write(
            t.join("tests").join("test.sh"),
            "#!/bin/bash\necho 1 > /logs/verifier/reward.txt\n",
        )
        .unwrap();
        std::fs::write(t.join("solution").join("solve.sh"), "echo hi").unwrap();
        let tasks = load(dir.path()).unwrap();
        assert_eq!(tasks.len(), 1);
        let task = &tasks[0];
        assert_eq!(task.id, "hello-world");
        assert_eq!(task.agent_timeout_ms, 120_000);
        assert_eq!(task.evaluator.timeout_ms, 60_000);
        assert_eq!(task.environment.memory_mb, Some(2048));
        // The solution is not part of the task identity.
        let before = task.task_blake3.clone();
        std::fs::write(t.join("solution").join("solve.sh"), "echo changed").unwrap();
        assert_eq!(load_task(&t).unwrap().task_blake3, before);
        std::fs::write(t.join("tests").join("test.sh"), "changed").unwrap();
        assert_ne!(load_task(&t).unwrap().task_blake3, before);
    }
}
