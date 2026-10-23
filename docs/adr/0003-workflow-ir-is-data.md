# ADR 0003: The workflow IR is data, not code

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-06 |
| Decided by | Maintainer, 2026-10-23 ("accept for all") |

## Context

Users and agents will author workflows in Python, YAML and natural language ([`../VISION.md`](../VISION.md) section 7). Python-native orchestrators (Airflow, Prefect) treat the user's program as the workflow. That makes a workflow impossible to hash, diff or approve before it runs, and when an LLM writes the program, the model's output becomes the runtime. Zeus already decided that acquisition plans are data: JSON, no floating-point numbers, hashed with JCS and BLAKE3 so the hash can bind an approval, and a closed template language (Zeus `docs/ARCHITECTURE.md` section 8).

## Decision

1. **One IR.** A workflow is a versioned JSON document: a DAG of nodes. Each node names an operator by name and version, gives its configuration, declares its inputs from upstream outputs, and sets its policy (retries, budgets, partitioning, trigger). Like a Zeus plan, the IR contains no floating-point numbers and has a stable JCS and BLAKE3 **workflow hash**.
2. **Every authoring layer compiles to the IR.** YAML maps one to one. The Python SDK *builds* IR and is not executed by the runtime. Natural language goes prompt → LLM → candidate IR → validator → plan and diff → human approval → runtime. There is never a path from prompt to an arbitrary shell session.
3. **Code lives in operators, not in the IR.** The IR's only expression forms are Zeus's template language (closed filter set) and a closed predicate set for routing edges (for example on MIME type, size or a declared field). Code-executing operators (Python, shell, SQL, dbt) reference their code by content hash. Adding or changing one requires a human approval bound to the new workflow hash.
4. **Acquisition nodes reuse the Zeus plan vocabulary.** The `acquire` operator's configuration (sources, pagination, selectors, filters, politeness, budgets) uses the same schema as a Zeus acquisition plan wherever possible. That keeps one vocabulary across products and lets the pure plan-IR code move to `zeus-core` when Ogun needs it (ADR 0002 decision 8). A `zeus.session_acquire` node carries a Zeus plan or job spec, and its hash is part of the workflow hash.
5. **Versioned and diffable.** Every change produces a new workflow version. A plan diff (nodes, operators, sources, budgets and code hashes added or removed) is what a human reviews.

## Consequences

- Workflows can be approved, reproduced, replayed and exported (to OpenLineage, or to other orchestrators) because they are data.
- The SDK has to be designed as a builder. Arbitrary Python control flow at definition time is allowed; control flow at run time has to be expressed as IR (routing, map, partitions).
- Some dynamic workflows (an agent that discovers new task types at run time) need an explicit, approved "plan amendment" step instead of silent self-modification.

## Alternatives considered

- **Python as the runtime** (Airflow or Prefect style): rejected, because it cannot be hashed or approved before running, and LLM-written code would execute directly.
- **A general-purpose DSL with loops and calls**: rejected; it amounts to code and brings the same problems.
- **No IR, just MCP calls by an agent**: rejected; nothing durable, reproducible or schedulable remains.
