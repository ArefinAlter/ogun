# O-06. Benchmark profile

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-00, O-01, O-05, O-12 |
| Decided by | ADR 0005 (benchmark runs are a run profile), ADR 0004 decision 2 |
| Implemented by | `ogun-bench` (library and the `ogun-bench` binary) |

## 1. Purpose

Ogun can measure agents on existing benchmarks: Terminal-Bench 2.0 and other tasks in the Harbor format, Spider 2.0-DBT, and ADE-bench. A benchmark is a controlled collection of runs (`Run = Workflow + Environment + Agent + Policy + Inputs + Evaluator`), not a separate system. This spec fixes the environment, evaluator, agent-run, cache, adapter and matrix contracts.

Product mode optimises for finishing the task; benchmark mode measures the agent. The profile therefore switches off help: one attempt per run (no retries beyond what the benchmark defines), no repair, no help from other agents, a fresh workspace, memory and environment for every run, and no cache unless a person turns one on.

## 2. Isolation rules

These are requirements, not defaults to be tuned away:

1. A benchmark run MUST NOT touch the user's Ogun or Zeus artifact stores, `ogund`'s or `zeusd`'s state, `zeusd` itself, or browser sessions. The benchmark state directory and the results directory MUST NOT be inside, or contain, Ogun's or Zeus's state directory (`OGUN_HOME`, `ZEUS_STATE_DIR` and the platform defaults); `ogun-bench` refuses them with `bench.isolation_unavailable`.
2. Agents and evaluators start with a cleared environment. No variable named `OGUN_*` or `ZEUS_*` (other than the `OGUN_BENCH_*` variables of section 7), no `PATH` or `HOME` override, and no credential reaches them except the model credentials a person listed in the agent's `pass_env`. Those are passed by name (`exec -e NAME`), never on a command line, and their values are scrubbed from the trajectory (O-05 section 7).
3. The network is denied by default, separately for the agent phase and the evaluator phase. Only a person, in the matrix, allows it (`agent_network`, `evaluator_network`). A run that cannot enforce the denial is refused, not run with less.
4. The workspace is read-write, `/benchmark` read-only, the evaluator's files (`/tests`) appear only after the agent finished, and nothing else of the host is visible in a container.
5. `ogun-bench` never connects to `ogund` or `zeusd` and reads no Ogun or Zeus credential file.

## 3. Runtimes

| Runtime | Isolation | Network denial | Platforms (O-12) |
|---|---|---|---|
| `docker`, `podman` (default `docker`) | One container per run: `--cap-drop ALL`, `--security-opt no-new-privileges`, a process limit (default 512), optional CPU and memory limits | `--network none`, or an internal network without egress when the evaluator phase may use the network | Linux full; Windows through Docker or WSL 2; macOS through Docker or Lima |
| `local` | A temporary directory; commands run as child processes with a cleared environment and their own process group | A user and network namespace (`unshare -r -n`, Linux). Elsewhere the run is refused unless `allow_unisolated_local` is set, and then results say `unisolated_local` | Development and trusted tasks only |

Tasks that ship a container image (Harbor) need a container runtime; on `local` they are refused with `bench.task_unsupported`. MicroVMs are later (ADR 0005).

## 4. Environment

### 4.1 Layout

Every agent run gets a new environment (`env_`) under the benchmark state directory:

```text
<state>/envs/<env_id>/
  environment.json   runtime, container name, working directory, host paths
  workspace/         read-write: the task's working directory (mounted at it in a container)
  benchmark/         read-only at /benchmark: instruction.md and the files the task lets the agent read
  logs/agent/        the agent's own logs and ogun-trajectory.jsonl
  logs/verifier/     the evaluator's output (Harbor writes reward.txt here)
  tests/             local runtime only: the evaluator's files, staged after the agent
  cache/             cache mode cold
```

### 4.2 Lifecycle

1. Build or pull the image: a task's `Dockerfile` is built once and tagged `ogun-bench/<benchmark>-<task>:<first 12 digits of the task BLAKE3>`; a named image is pulled when absent. Building and pulling may use the network: they prepare the environment and are not part of the measured run.
2. Seed the workspace with the image's working directory (Harbor images put the task's files there), then copy the task's fixtures over it.
3. Start the container (or prepare the local directory), run the task's setup script if it has one (copied in, run, removed), and snapshot the workspace.
4. Run the agent (section 7), snapshot again, record the differences as `file.created`, `file.patch` and `file.deleted`, and the digests of DuckDB files before and after as `db.state`.
5. Remove any `/tests` the agent created, stage the evaluator's files there, apply the evaluator network policy (open egress when allowed, close it when denied), run the evaluator (section 6).
6. Copy `logs/` into the results, remove the container and its network, and delete the environment directory unless `keep_environments` is set.

A command that exceeds its time limit is killed; in a container every process is stopped by restarting the container, which keeps its file system for the evaluator.

### 4.3 Environment digest

`environment_blake3` is the BLAKE3 of the JCS form of `{runtime, image_id, workdir, agent_network, evaluator_network, cpus, memory_mb, pids_limit, cache_mode, isolation, fixture_blake3}`, where `image_id` is the runtime's content id of the image and `fixture_blake3` the tree digest of the workspace before the agent starts. Credential values and host paths are not part of it.

### 4.4 Cache modes (ADR 0005 decision 2)

| `cache_mode` | `/cache` |
|---|---|
| `disabled` (default) | absent |
| `benchmark_provided` | the task's own `cache/` directory, read-only |
| `cold` | empty, for this run only |
| `warm` | kept under `<state>/warm/<task BLAKE3>` across runs of the same task |

## 5. Tasks

A task is adapter-neutral: benchmark family, id, version, instruction (untrusted text), environment needs (image or build context, working directory, CPU, memory, build time limit), fixtures, files for `/benchmark`, files for `/tests`, an optional setup script, the evaluator and the agent's time limit. `task_blake3` is the BLAKE3 over the task's named parts: instruction, task description file, environment, fixtures and evaluator files. Reference solutions are not part of it and are never shown to the agent.

## 6. Evaluators

An evaluator (`evl_`) is a command that runs unmodified in the environment after the agent. Its command may use the placeholders `{workspace}`, `{logs}`, `{tests}`, `{benchmark}` and `{task_id}`, which name the paths as the command sees them. Its verdict comes from:

| `result.kind` | Verdict |
|---|---|
| `reward_file` | The first existing file of `paths` (relative to `logs/`) holding a number, or `{"reward": n}` |
| `json_file` | A JSON document `{"passed": bool, "score": n}` or `{"score_ppm": n}` |
| `stdout` | The last line of standard output that is a JSON object or a number |
| `exit_code` | 0 passes with a full score; anything else fails with zero |

Scores are integers in parts per million (`score_ppm`, 0 to 1,000,000). Decimal scores are converted once, at this boundary, by decimal string arithmetic and clamped; Ogun records no floats. A run passes when `score_ppm >= pass_threshold_ppm` (default 1,000,000), or as the document's `passed` says. An evaluator that writes no readable verdict is `bench.evaluator_failed` and the run's outcome is `error`; one that exceeds its time limit fails the run. The evaluator's full result is kept as `evaluator.json` with its BLAKE3 in the agent-run record.

## 7. Agents and agent runs

An agent is a command a person configured in the matrix, never text from a prompt or a task:

| Member | Meaning |
|---|---|
| `name`, `version`, `model` | Identity |
| `placement` | `container` (default): the command runs in the environment. `host`: it runs on the host with a cleared environment and reaches the environment only through `ogun-bench exec`, so it can call its model provider while the environment has no network |
| `command` | Placeholders `{instruction_file}`, `{workspace}`, `{logs}`, `{benchmark}`, `{cache}`, `{seed}`, `{task_id}`, `{trajectory_file}`, `{model}` |
| `pass_env` | Names of model credential variables passed through |
| `env` | Non-secret variables |
| `timeout_ms` | Overrides the task's limit |
| `prompt_file`, `tools_file` | Hashed into the record (`prompt_blake3`, `tools_blake3`) |

The agent receives `OGUN_BENCH_TASK`, `OGUN_BENCH_SEED`, `OGUN_BENCH_INSTRUCTION` and `OGUN_BENCH_TRAJECTORY`; a host agent also `OGUN_BENCH_EXEC_BIN` and `OGUN_BENCH_ENV_DIR`, and runs commands with `$OGUN_BENCH_EXEC_BIN exec --env-dir $OGUN_BENCH_ENV_DIR -- <argv>`, which records each command as `tool.shell` and `tool.result` in the agent's log.

An agent MAY write its own events, one JSON object per line, to `OGUN_BENCH_TRAJECTORY` (`logs/agent/ogun-trajectory.jsonl`), using the agent-reportable types of O-05 section 7 (for example `{"type":"usage","input_tokens":120,"output_tokens":40,"cost_usd_micro":900}`). The harness ingests them after the agent exits.

Every run is one `agr_` record (O-05 section 6) with its trajectory. Outcomes: `completed` (the evaluator gave a verdict), `timed_out` (the agent hit its limit; the evaluator still runs), `failed`, `cancelled`, and `error` when the harness could not run the task (environment, image or evaluator failure). Errors are counted separately and do not enter pass rates.

## 8. Result cache

| `result_cache` | Behaviour |
|---|---|
| `off` (default) | Every run executes |
| `read` | A cached result is reused; nothing new is stored |
| `read_write` | A cached result is reused; new results are stored |

The key is the BLAKE3 of the JCS form of `{task_blake3, environment_blake3, agent_config_blake3, seed, evaluator}`: a result is reused only for the same task content, environment, agent configuration, seed and evaluator. Runs with outcome `error` are not cached. A reused result keeps its original `agr_` id and record, with `cached: true`. The cache lives in `<state>/result-cache`, never in a user's store.

## 9. Adapters

Ogun reads each benchmark's own layout and keeps its official evaluator (ADR 0005 decision 4). The layouts below are what this build reads. **They are unverified against the upstream repositories** (research OR3 is planned); an adapter that meets something else fails with `bench.task_invalid` naming the missing file. A matrix can replace an adapter's evaluator command and result source per benchmark (`evaluator_command`, `evaluator_result`).

### 9.1 Harbor (`harbor`; Terminal-Bench 2.0)

A task directory holds `instruction.md`, `task.toml` (`version`; `[agent] timeout_sec`; `[verifier] timeout_sec`; `[environment] build_timeout_sec`, `docker_image`, `cpus`, `memory` such as `"2G"` or `memory_mb`), `environment/Dockerfile`, `tests/test.sh` and `solution/solve.sh`. A path holding `task.toml` is one task; otherwise every subdirectory with one is. The working directory is the image's `WORKDIR` (default `/app`). The evaluator is `bash /tests/test.sh`, reading `/logs/verifier/reward.txt` or `reward.json`. Defaults: agent 1,800 s, verifier 600 s. Environments given only as docker-compose files are not supported yet (`bench.task_unsupported`). Harbor's `test.sh` scripts commonly install their test runner from the network; such tasks need `evaluator_network: allowed`.

### 9.2 Spider 2.0-DBT (`spider2_dbt`)

The `spider2-dbt` directory holds `examples/spider2-dbt.jsonl` (`{"instance_id", "instruction"}` per line), `examples/<instance_id>/` (the dbt project and its DuckDB file, copied into the workspace at `/workspace`) and `evaluation_suite/` (with `gold/<instance_id>/`, staged at `/tests` after the agent). The tasks name no image: the matrix's `environment.image` must provide dbt with DuckDB. The default evaluator copies the workspace to `logs/verifier/results/<instance_id>` and runs `python3 evaluate.py --mode dbt --result_dir ...` from `/tests`, reading its last JSON object or number on standard output; both the command line and the output are assumptions to verify.

### 9.3 ADE-bench (`ade_bench`)

The checkout holds `tasks/<task_id>/task.yaml` (`task_id`; `prompts` with a `base` prompt, or `instruction` or `description`; `variants` with `db_type`, `db_name`, `project_type`, `project_name`), `tasks/<task_id>/tests/*.sql`, an optional `tasks/<task_id>/setup.sh`, `shared/projects/<project_type>/<project_name>/` and `shared/databases/duckdb/<db_name>.duckdb`. Only DuckDB variants run (Snowflake variants need credentials a benchmark run must not have). The project and the database are the workspace at `/workspace`; `setup.sh` runs before the agent; the default evaluator copies the tests into `tests/ogun_eval/` and runs `dbt test --select path:tests/ogun_eval`, passing on exit code 0.

## 10. Matrices

A matrix (`matrix.json`) lists `benchmarks` (adapter, path, optional task ids, `limit`, evaluator and time-limit overrides), `agents`, `seeds` (default `[0]`), `environment` (runtime, networks, limits, `cache_mode`, `image`, `allow_unisolated_local`), `result_cache`, `keep_environments` and `lineage_namespace`. Unknown members are refused.

Runs execute one at a time in task, agent, seed order. The results directory holds `matrix.json`, `results.json` (rewritten after every run, so an interrupted matrix keeps its partial results), `results.md`, `lineage.jsonl` (one OpenLineage event per agent run, O-05) and `runs/<agr_id>/` with `record.json`, `trajectory.jsonl`, `evaluator.json` and `logs/`.

Each row of `results.json` has the run's agent, task, seed, outcome, `passed`, `score_ppm`, times, token and cost totals, tool calls, failed tool calls, repeated commands, files touched, `cached`, `isolation` and the harness `error_code`. Each per-agent summary has runs, errors, scored runs, passed, `pass_rate_ppm` (passed per scored run), `mean_score_ppm`, timeouts, totals, `cost_per_success_usd_micro` (VISION section 12: C_s, total cost over passed runs), `mean_agent_ms`, tool calls and `tool_efficiency_ppm` (tool calls that neither failed nor repeated a command, per tool call). Harness gain and tool ablation are computed across matrices that differ in one configuration; this build records what they need but does not compute them yet.

```json
{
  "name": "dbt-agents",
  "benchmarks": [
    { "adapter": "ade_bench", "path": "/data/ade-bench", "limit": 5 },
    { "adapter": "harbor", "path": "/data/terminal-bench-2/tasks", "tasks": ["hello-world"] }
  ],
  "agents": [
    { "name": "my-agent", "version": "0.3.0", "model": "example-model", "placement": "host",
      "command": ["my-agent", "--instruction", "{instruction_file}", "--seed", "{seed}"],
      "pass_env": ["EXAMPLE_API_KEY"] }
  ],
  "seeds": [1, 2, 3],
  "environment": { "runtime": "docker", "image": "ogun-bench/dbt-duckdb:local", "memory_mb": 4096, "evaluator_network": "allowed" },
  "result_cache": "off"
}
```

## 11. The `ogun-bench` binary

| Command | Does |
|---|---|
| `ogun-bench run --matrix <file> --out <dir>` | Runs a matrix and prints the Markdown table |
| `ogun-bench tasks --adapter <a> --path <p> [--json]` | Lists what an adapter reads |
| `ogun-bench report --results <dir>` | Prints `results.md` again from `results.json` |
| `ogun-bench replay <trajectory.jsonl> [--summary]` | Verifies the chain and prints the events and their summary |
| `ogun-bench exec --env-dir <dir> -- <argv>` | Runs a command in an environment (host agents) |

`--state-dir` (or `OGUN_BENCH_STATE_DIR`) selects the benchmark state directory; the default is `$XDG_STATE_HOME/ogun-bench` on Linux and `OgunBench` under the local data directory elsewhere. Logs go to standard error (`OGUN_BENCH_LOG` filters them).

## 12. Error codes

Registered in O-00 section 5.2:

| Code | Class | Retryable | Raised when |
|---|---|---|---|
| `bench.task_invalid` | `invalid_request` | no | A task directory does not match its adapter's format. |
| `bench.task_unsupported` | `unsupported` | no | The task needs something this build does not do (docker-compose environments, a container image on the local runtime, no image at all). |
| `bench.isolation_unavailable` | `policy_denied` | no | The isolation section 2 requires cannot be enforced, or the state directory overlaps Ogun's or Zeus's. |
| `bench.environment_failed` | `unavailable` | yes | The container runtime or a local process failed. |
| `bench.evaluator_failed` | `internal` | no | The evaluator produced no readable verdict. |
| `bench.spec_invalid` | `invalid_request` | no | A matrix or agent specification is invalid. |

## 13. Open issues

1. Verify the three adapters against the upstream repositories (research OR3) and record the evidence; until then results carry the adapter's assumptions.
2. Parallel runs (a bounded number of environments at once) and resumable matrices that skip rows already in `results.json`.
3. docker-compose Harbor environments (sidecar services such as databases).
4. Live ingestion of agent-reported events, so `at` and `elapsed_ms` reflect the agent's own timing.
5. Harness gain and tool ablation across matrices (VISION section 12), and edit efficiency, which needs task-aware heuristics.
6. Running benchmark matrices from `ogund` as Ogun runs, so a benchmark row is also a node instance with lineage in the run store; today `ogun-bench` is standalone by design (ADR 0005 consequence 2).
7. The container path has not been exercised end to end in CI yet (no container daemon on the build machine); the local runtime and the adapters' parsers are tested.
