# O-04. Scheduling: triggers, partitions, backfills and budgets

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-01, O-02 |
| Implemented by | `ogun-sched` (pure logic); the daemon loop that drives it is not implemented yet |

## 1. Purpose

This spec says when runs start, which partitions they cover, and how spending is limited while they run. The rules here are pure functions of the workflow, the clock and the run store. The daemon owns the clock and feeds it in as Unix milliseconds, so every decision can be replayed in a test.

## 2. Triggers

### 2.1 Cron

A `cron` trigger fires at every minute that matches its five fields, evaluated in UTC. The semantics follow Vixie cron:

- `*` means every value in the field's range.
- A list, range or `/step` selects values from that range.
- Day of week runs from 0 to 7, and both 0 and 7 mean Sunday.
- If day of month and day of week are both restricted (neither is `*`), a day matches when **either** field matches.
- The next fire time is the first matching whole minute **strictly after** the given instant.
- The scheduler searches eight years ahead. An expression that never matches in that window (for example `0 0 31 2 *`) never fires.

### 2.2 Interval

An `interval` trigger fires `interval_ms` after the previous run of the workflow started. If that moment has already passed, it fires now. If the workflow has never run, it fires now.

### 2.3 Manual

A `manual` trigger never fires on its own. A run starts only when a person or an agent asks for one, within the agent's permissions.

### 2.4 Next start

The next start of a workflow is the earliest next fire time over all of its triggers. Missed fire times are not replayed one by one: after downtime, the scheduler starts one run, and the partitions that run covers make up for what was missed (section 3).

## 3. Partitions

### 3.1 Keys and windows

| Declaration | Key | Window |
|---|---|---|
| `time`, `day` | `YYYY-MM-DD` | `[00:00, 24:00)` UTC of that day |
| `time`, `month` | `YYYY-MM` | from the first of the month at 00:00 UTC to the first of the next month |
| `static` | the key itself | none |

### 3.2 Completeness

A time partition is **complete** when its window has ended. The scheduler never runs a partition whose window is still open. That partition's data cannot all exist yet, and continuous sources (O-09) handle open windows differently.

The partitions of a workflow at time `now` are the complete keys from `start` to `end`, inclusive. When `end` is absent, the range runs to the last complete key. Static partitions are always complete.

### 3.3 Scheduled runs

A scheduled run of a partitioned workflow covers every complete partition that has not yet succeeded under the current workflow hash. A new workflow version therefore makes every partition due again.

This is deliberate: the artifacts of the old version stay where they are, and the new version produces new artifacts with their own lineage. To avoid recomputing history, set `start` to a later key, or use a backfill over a narrower range.

## 4. Backfills

A backfill is a run over an explicit range `[from, to]` of partition keys.

1. **Validity.** Both keys must be of the declared form. `from` must not come before `start`, and the range must not be reversed.
2. **Capping.** The range is capped at the last complete partition.
3. **Size.** A range may hold at most 100 000 partitions. Larger backfills are split.
4. **Up-to-date partitions.** A partition that already succeeded under the same workflow hash is `skipped` with cause `up_to_date` (O-01). Every other partition is run.
5. **Lanes.** The partitions to run are dealt round-robin into lanes, at most the node's `policy.concurrency`, so the earliest partitions finish first.
6. **Crash recovery.** Partitions commit independently. If the daemon is killed during a backfill, it resumes only the partitions that had not succeeded (O-01 section 4). This is the phase O1 exit test.

## 5. Budgets

### 5.1 Scopes

Workflow `budgets` apply to each run, and to each partition when the workflow is partitioned. A node's `policy.budgets` apply to that node within the same run or partition. Every amount is an integer: bytes, files, milliseconds or micro-dollars.

### 5.2 Reservations

Work reserves budget before it starts and commits the amount actually used when it ends.

1. **What is available.** For each resource, available = limit − committed use − open reservations. An unlimited resource always has room.
2. **Refusal.** A reservation larger than what is available is refused. Nothing is reserved.
3. **Over-reservation.** A commit may exceed its reservation, for example when a download turns out larger than declared. The overrun counts against later reservations.
4. **Releasing.** Releasing a reservation returns its amount unused.
5. **Exhaustion.** A scope is exhausted when committed use of any limited resource reaches its limit.

Model nodes require a workflow or node micro-dollar limit. The daemon injects the remaining reservation into the operator; workflow configuration cannot override it. Reservations and dispatch are committed together and survive restarts. If an interrupted request's bill is unknown, its reserved bound counts as used. A provider failure with an unknown bill also consumes its admitted bound before fallback, even if a later provider succeeds.

### 5.3 When a reservation is refused

What happens depends on the node's `policy.approval`:

- `over_budget`: the node moves to `waiting_for_user` (cause `approval_required`). A person may approve a raised limit. That approval is bound to the request hash of the new limit.
- `none` or `always`: the node fails with `run.budget_exhausted`.

An agent can never raise a budget.

## 6. Open issues

1. Time zones: triggers and partitions are UTC only. Local-time schedules need a rule for daylight-saving gaps first.
2. Data and condition triggers (VISION section 5) are not in IR v0.
3. Should a scheduled run that finds hundreds of due partitions start them all, or cap them and leave the rest for the next run? Today every due partition runs, limited only by `policy.concurrency`.
