//! Evaluators (`evl_`, O-06 section 6): commands that score a run, run unmodified in the environment after the
//! agent finished.
//!
//! A result comes from one of four places: a reward file (Harbor writes a number to `/logs/verifier/reward.txt`,
//! or `{"reward": n}` to `reward.json`), a JSON file, the last JSON object or number on standard output, or the
//! exit code. External tools report scores as decimals; Ogun records only integers, so a score is converted once,
//! at this boundary, to parts per million (`score_ppm`, 0 to 1,000,000) and the evaluator's full output is kept
//! beside the trajectory with its BLAKE3.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::env::{Environment, ExecOptions, expand};
use crate::{BenchError, BenchResult};

/// One million: a full score.
pub const FULL_SCORE_PPM: u32 = 1_000_000;

/// Where the verdict is read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResultSource {
    /// A file under the environment's logs directory holding a number, or `{"reward": n}`.
    RewardFile {
        /// Paths relative to the logs directory, tried in order (`verifier/reward.txt`, `verifier/reward.json`).
        paths: Vec<String>,
    },
    /// A JSON document `{"passed": bool, "score": n}` (or `score_ppm`) under the logs directory.
    JsonFile {
        /// Path relative to the logs directory.
        path: String,
    },
    /// The last line of standard output that is a JSON object or a number.
    Stdout,
    /// Exit code 0 passes with a full score; anything else fails with zero.
    ExitCode,
}

fn default_threshold() -> u32 {
    FULL_SCORE_PPM
}

/// How a task is scored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluatorSpec {
    /// Name, for example `harbor.verifier`.
    pub name: String,
    /// Version: the benchmark's version, or the evaluator's own.
    pub version: String,
    /// Command, with `{workspace}`, `{logs}`, `{tests}`, `{benchmark}`, `{task_id}` placeholders.
    pub command: Vec<String>,
    /// Time limit.
    pub timeout_ms: u64,
    /// Where the verdict is read.
    pub result: ResultSource,
    /// A run passes when its score is at least this (default: a full score).
    #[serde(default = "default_threshold")]
    pub pass_threshold_ppm: u32,
}

impl EvaluatorSpec {
    /// `name@version`.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

/// An evaluator's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalResult {
    /// Pass or fail.
    pub passed: bool,
    /// Score in parts per million.
    pub score_ppm: u32,
    /// Exit code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Wall time.
    pub duration_ms: u64,
    /// The evaluator stopped at its time limit.
    pub timed_out: bool,
    /// The raw result document, as read (JSON), for the results directory.
    pub raw: Value,
}

/// Converts a decimal score text (`1`, `0.5`, `1e0`) to parts per million, clamped to 0 to 1,000,000. The
/// conversion is decimal string arithmetic, so no float is involved.
#[must_use]
pub fn ppm_from_decimal(text: &str) -> Option<u32> {
    let t = text.trim();
    if let Ok(n) = t.parse::<i64>() {
        return Some(if n >= 1 { FULL_SCORE_PPM } else { 0 });
    }
    let (mantissa, exp) = match t.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i32>().ok()?),
        None => (t, 0),
    };
    let negative = mantissa.starts_with('-');
    let mantissa = mantissa.trim_start_matches(['-', '+']);
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if !int.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
        || (int.is_empty() && frac.is_empty())
    {
        return None;
    }
    if negative {
        return Some(0);
    }
    // value = digits * 10^(exp - frac.len()); ppm = value * 10^6.
    let digits = format!("{int}{frac}");
    let digits = digits.trim_start_matches('0');
    let shift = exp + 6 - i32::try_from(frac.len()).ok()?;
    if digits.is_empty() {
        return Some(0);
    }
    let value: u128 = if shift >= 0 {
        let base: u128 = digits.get(..digits.len().min(30))?.parse().ok()?;
        if digits.len() > 30 || shift > 30 {
            return Some(FULL_SCORE_PPM);
        }
        base.saturating_mul(10u128.checked_pow(u32::try_from(shift).ok()?)?)
    } else {
        let cut = usize::try_from(-shift).ok()?;
        if cut >= digits.len() {
            0
        } else {
            digits[..digits.len() - cut].parse().ok()?
        }
    };
    Some(u32::try_from(value.min(u128::from(FULL_SCORE_PPM))).unwrap_or(FULL_SCORE_PPM))
}

fn ppm_from_value(v: &Value) -> Option<u32> {
    match v {
        Value::Number(n) => ppm_from_decimal(&n.to_string()),
        Value::Bool(b) => Some(if *b { FULL_SCORE_PPM } else { 0 }),
        Value::String(s) => ppm_from_decimal(s),
        _ => None,
    }
}

/// Reads a verdict document: `{"score_ppm": n}`, `{"score": n}`, `{"reward": n}` and/or `{"passed": b}`.
fn verdict(doc: &Value, threshold: u32) -> Option<(bool, u32)> {
    if let Some(ppm) = ppm_from_value(doc) {
        return Some((ppm >= threshold, ppm));
    }
    let obj = doc.as_object()?;
    let ppm = obj
        .get("score_ppm")
        .and_then(Value::as_u64)
        .map(|n| u32::try_from(n.min(u64::from(FULL_SCORE_PPM))).unwrap_or(FULL_SCORE_PPM))
        .or_else(|| obj.get("score").and_then(ppm_from_value))
        .or_else(|| obj.get("reward").and_then(ppm_from_value));
    let passed = obj.get("passed").and_then(Value::as_bool);
    match (passed, ppm) {
        (Some(p), Some(s)) => Some((p, s)),
        (Some(p), None) => Some((p, if p { FULL_SCORE_PPM } else { 0 })),
        (None, Some(s)) => Some((s >= threshold, s)),
        (None, None) => None,
    }
}

fn read_doc(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    let t = text.trim();
    serde_json::from_str(t)
        .ok()
        .or_else(|| Some(Value::String(t.to_owned())))
}

/// Runs the evaluator in `env` and reads its verdict.
///
/// # Errors
///
/// [`BenchError::Evaluator`] when no verdict can be read (a missing reward file, unparseable output);
/// [`BenchError::Environment`] when the command cannot start.
pub async fn evaluate(
    spec: &EvaluatorSpec,
    env: &Environment,
    task_id: &str,
) -> BenchResult<EvalResult> {
    let p = env.paths();
    let mut vars = BTreeMap::new();
    vars.insert("workspace", p.workspace);
    vars.insert("logs", p.logs);
    vars.insert("tests", p.tests);
    vars.insert("benchmark", p.benchmark);
    vars.insert("task_id", task_id.to_owned());
    let argv = expand(&spec.command, &vars);
    let out = env
        .exec(
            &argv,
            &ExecOptions {
                timeout_ms: spec.timeout_ms,
                ..ExecOptions::default()
            },
        )
        .await?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let found = match &spec.result {
        ResultSource::RewardFile { paths } => {
            paths.iter().find_map(|r| read_doc(&env.logs.join(r)))
        }
        ResultSource::JsonFile { path } => read_doc(&env.logs.join(path)),
        ResultSource::Stdout => stdout
            .lines()
            .rev()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .find_map(|l| {
                serde_json::from_str::<Value>(l)
                    .ok()
                    .filter(|v| v.is_object() || v.is_number())
            }),
        ResultSource::ExitCode => Some(Value::Bool(out.exit_code == Some(0))),
    };
    let doc = match (found, out.timed_out) {
        (Some(d), _) => d,
        (None, true) => Value::Bool(false),
        (None, false) => {
            return Err(BenchError::Evaluator(format!(
                "{} produced no verdict (exit code {:?})",
                spec.label(),
                out.exit_code
            )));
        }
    };
    let (passed, score_ppm) = verdict(&doc, spec.pass_threshold_ppm).ok_or_else(|| {
        BenchError::Evaluator(format!("{} wrote a verdict Ogun cannot read", spec.label()))
    })?;
    // Keep the document without floats: numbers become their decimal text.
    let raw = serde_json::json!({ "verdict": stringify_numbers(doc), "exit_code": out.exit_code });
    Ok(EvalResult {
        passed: passed && !out.timed_out,
        score_ppm: if out.timed_out { 0 } else { score_ppm },
        exit_code: out.exit_code,
        duration_ms: out.duration_ms,
        timed_out: out.timed_out,
        raw,
    })
}

/// Replaces every non-integer number with its decimal text, so the document can be hashed (O-00).
#[must_use]
pub fn stringify_numbers(v: Value) -> Value {
    match v {
        Value::Number(n) if n.as_i64().is_none() && n.as_u64().is_none() => {
            Value::String(n.to_string())
        }
        Value::Array(a) => Value::Array(a.into_iter().map(stringify_numbers).collect()),
        Value::Object(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| (k, stringify_numbers(v)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_scores_become_ppm_without_floats() {
        assert_eq!(ppm_from_decimal("1"), Some(1_000_000));
        assert_eq!(ppm_from_decimal("1.0"), Some(1_000_000));
        assert_eq!(ppm_from_decimal("0"), Some(0));
        assert_eq!(ppm_from_decimal("0.5"), Some(500_000));
        assert_eq!(ppm_from_decimal("0.1234567"), Some(123_456));
        assert_eq!(ppm_from_decimal("5e-1"), Some(500_000));
        assert_eq!(ppm_from_decimal("2.5"), Some(1_000_000));
        assert_eq!(ppm_from_decimal("-0.3"), Some(0));
        assert_eq!(ppm_from_decimal("abc"), None);
        assert_eq!(
            verdict(&serde_json::json!({"passed": true}), FULL_SCORE_PPM),
            Some((true, 1_000_000))
        );
        assert_eq!(
            verdict(&serde_json::json!({"reward": 0.25}), 200_000),
            Some((true, 250_000))
        );
        assert_eq!(
            verdict(&Value::String("0.0".into()), FULL_SCORE_PPM),
            Some((false, 0))
        );
        let raw = stringify_numbers(serde_json::json!({"reward": 0.25, "n": 3}));
        zeus_verify::canonical::check_hashable(&raw).unwrap();
    }
}
