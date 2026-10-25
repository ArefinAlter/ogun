# O-17. Agent threads

| | |
|---|---|
| Status | Draft 1 (2026-10-25) |
| Depends on | O-03, O-05, O-07, O-13, O-14, O-15, O-16 |
| Decided by | ADR 0014 (Proposed) |
| Implemented by | `apps/studio/web/src/lib/agentTools.ts` (the tool table), `src/lib/markdown.ts` (how an agent's words are shown), `src/views/Agent.tsx` and `src/views/agent` (the screen), `src/views/settings/Agent.tsx`; the scripted demo `src/demo/agent.ts` implements sections 5 to 7 for demonstration only. No daemon implements them yet. |

## 1. Purpose

A person gives Ogun's agent a data-engineering task in a conversation. The agent reads the daemon through a closed set of
tools, explains, validates and plans, and proposes changes as cards. A person presses whatever changes anything. This
spec adds no way to run code, to approve or to hold a secret.

## 2. Objects

| Object | Members |
|---|---|
| Thread | `thread_id` (`thr_`), `title`, `state` (`idle`, `running`, `waiting`), `access`, `model`, `created_at`, `updated_at`, `item_count`, `items[]`, `mentions[]` |
| Item | `id` (`itm_`), `at`, and one of the kinds below |
| Mention | `{kind: workflow\|run\|connector\|approval, id, label}`: what a person pointed at with `@` |

`state` is `waiting` while a card the agent proposed has not been pressed: a valid proposal not stored, a backfill not
started, a retry not started.

Item kinds:

| Kind | Members | Notes |
|---|---|---|
| `user` | `text`, `mentions[]` | A person's message. |
| `assistant` | `text`, `streaming` | The agent's words: untrusted text, a Markdown subset (section 8). |
| `tool` | `tool`, `summary`, `input?`, `output?`, `state` (`running`, `ok`, `error`, `denied`), `ms?` | One tool call. `output` is the daemon's answer, bounded. |
| `card` | `card` | Section 6. |
| `notice` | `tone` (`info`, `warn`), `text` | The daemon's own line: stopped, access changed. |

## 3. Access

| Level | May also |
|---|---|
| `read` | Read, validate, plan. Changes nothing. |
| `author` | Store a workflow version (`workflows.put`). A version that changes code still creates a code approval. |
| `operate` | Start and cancel runs. Egress, code and budget approvals still wait for a person. |

A thread starts at the settings' `default_access` (`read` unless a person chose otherwise). Only a person raises it, in the
interface; there is no tool that does.

## 4. The tool table

Normative, and mirrored by `agentTools.ts`.

| Tool | Least access | Daemon method |
|---|---|---|
| `status.get` | read | `daemon.status` |
| `workflows.list`, `workflows.get` | read | `workflows.list`, `workflows.get` |
| `workflows.validate` | read | `workflows.validate` |
| `runs.list`, `runs.get` | read | `runs.list`, `runs.get` |
| `partitions.status`, `backfills.plan` | read | `partitions.status`, `backfills.plan` |
| `artifacts.list`, `artifacts.read` | read | `artifacts.list`, `artifacts.get` (a preview of at most 64 KiB) |
| `lineage.get` | read | `lineage.get` |
| `approvals.list` | read | `approvals.list`, `approvals.get` |
| `connectors.search` | read | the O-16 catalogue |
| `workflows.put` | author | `workflows.put` |
| `runs.start`, `runs.cancel` | operate | `runs.start` (with an idempotency key), `runs.cancel` |

A call is refused, with item state `denied` and a reason, when the tool does not exist, when `tools_enabled` in the
settings omits it, or when the thread's access is below the tool's. What is never in the table, at any access:

1. a tool that decides an approval (`approvals.decide` is not callable by an `agent` credential, O-15 sections 4.3 and 6);
2. a tool that returns a secret value (`secrets.list` returns names, and the agent has no tool for even that);
3. a tool that runs a shell command or any code that is not a stored, approved operator;
4. a tool that reaches a host.

`agentTools.test.mjs` fails if a tool name suggests any of these.

## 5. Methods (proposed daemon methods)

| Method | Parameters | Result | Who may call |
|---|---|---|---|
| `agent.models` | none | `models[]` (`id`, `label`, `provider`, `available`, `note?`) | human, agent |
| `agent.tools` | none | the table of section 4 | human, agent |
| `agent.settings.get`, `agent.settings.put` | patch, `idempotency_key` | `AgentSettings` | human |
| `agent.threads.list` | none | `threads[]` (summaries) | human |
| `agent.threads.create` | `access`, `model` | `Thread` | human |
| `agent.threads.get` | `thread_id` | `Thread` | human |
| `agent.threads.rename`, `agent.threads.delete` | `thread_id`, `title?` | `{}` | human |
| `agent.access.set` | `thread_id`, `access` | `{}` | human |
| `agent.send` | `thread_id`, `text`, `mentions[]` | `{}`; the answer streams as events | human |
| `agent.stop` | `thread_id` | `{}` | human |
| `agent.act` | `thread_id`, `item_id`, `action`, `idempotency_key` | `{}` | human |

`AgentSettings`: `instructions` (at most 8000 characters), `default_access`, `default_model`, `spend_cap_usd` (0 means no model call),
`tools_enabled[]`, `retention_days` (7, 30, 90 or 365). Thread changes arrive on the event stream (O-15 section 8) as
`agent.item` and `agent.thread` events, so a thread streams to every Studio that has it open.

`agent.act` actions: `store` (a `proposal` card), `start_backfill` (a `backfill` card), `retry` (a `retry` card). The
daemon executes the action with the **caller's** role as the call a person would make themselves (`workflows.put`,
`runs.start`), and refuses `agent.act` to an `agent` credential, so an agent can never press its own button.

## 6. Cards

A card is built by the daemon from a tool result, never from the agent's prose.

| Card | Made from | Action |
|---|---|---|
| `plan` | the agent's own steps | none |
| `proposal` | `workflows.validate` of a document: hash, diagnostics, what waits for a person | `store` |
| `backfill` | `backfills.plan`: range, to run, up to date | `start_backfill` |
| `retry` | `runs.get`: the failed node and its recorded cause | `retry` |
| `approvals` | `approvals.list` | none: each row links to the approval page |
| `run` | a run id the daemon returned | none |
| `table` | `artifacts.read` of a CSV | none |
| `connector` | the O-16 catalogue | none |

An approval is never decided in a thread. The `approvals` card has no decision control, and the requester's own words
(`untrusted_reason`) are not relayed into a thread: they are read on the approval page, marked as not verified.

## 7. Limits and secrets

1. Instructions and every message are checked against the redaction rules of Zeus constraint 3 and O-03 section 8.6
   before they reach a prompt. A value that looks like a secret (a key, a token, a private key block, a `password =`
   assignment, a URL query holding a credential) is **refused with an error, not masked**. Studio checks the same
   patterns on the Settings page so a person finds out before saving.
2. A thread's spend is counted in the spend ledger of O-03 section 8.9. At the cap the thread stops calling the model and
   says so; reads and cards from earlier turns stay.
3. A thread is private to its creator. On a hub it is stored in the daemon the hub serves and never synced to an edge.
4. Retention removes finished threads after `retention_days`. A thread holds questions and answers, never a secret.
5. Rate and size limits: 20 sends a minute per thread, a message of at most 16,000 characters, at most 12 mentions.

## 8. How the interface shows an agent's words

`markdown.ts` reads a subset (paragraphs, headings, lists, fenced code, quotes, pipe tables, `code`, strong, emphasis,
links) into a tree that is rendered node by node. Raw HTML stays text. A link survives only when it is an address inside
Studio (`#/...`) or an `https` address without credentials; any other becomes plain text with its address. A test pins this.

## 9. Conformance

```sh
cd apps/studio/web && npm test                                   # tool table, Markdown reader, identity, durations
python tools/ui-reference/smoke_studio.py --base http://localhost:4174   # the screen's flows on the scripted agent
```

## 10. Open issues

1. Where the loop runs: inside `ogund`, or as a child under the operator ABI (O-03) with the egress rule of a model
   operator. The second reuses `ogun-op-llm`, the child that already calls providers for the model operators, and keeps `ogund` free of an HTTP client for them.
2. Id prefixes `thr_` and `itm_` are not yet in O-00's table or Zeus 00 section 5.1.
3. The streaming shape on the event stream, and whether an `assistant` item may be edited in place or only appended.
4. Cancellation: whether `agent.stop` may leave a started `runs.start` running (it does today: a started run is the
   daemon's, not the thread's).
5. Prompt injection through tool results (a workflow description, an artifact preview). The boundary holds because the
   tool table cannot decide, read a secret or reach a host; red-team tests against a real model are still to write.
6. Several people on one hub: threads are private today; shared threads need an owner, a role check on `agent.act`, and
   a decision on who sees the spend.
7. Studio-web sends no workspace header yet, so a person in several workspaces has no way to choose (O-13 section 6).
