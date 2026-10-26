# O-17. Agent threads

| | |
|---|---|
| Status | Draft 1 (2026-10-25) |
| Depends on | O-03, O-05, O-07, O-13, O-14, O-15, O-16 |
| Decided by | ADR 0014 (Accepted by the maintainer's instruction of 2026-10-26) |
| Implemented by | `crates/ogun-daemon/src/studio/agent` (threads, the loop, the tool table, cards, provider adapters: sections 2 to 7 and 11); `apps/studio/web/src/lib/agentTools.ts` (the tool table as the interface knows it), `src/lib/markdown.ts` (how an agent's words are shown), `src/views/Agent.tsx` and `src/views/agent` (the screen), `src/views/settings/Agent.tsx`; the scripted backend `src/demo/agent.ts` still implements sections 5 to 7 for the hosted web build |

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

## 5. Methods (daemon methods)

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
| `agent.preview` | `thread_id?`, `text?` | what would be sent to the provider: `provider`, the host and model, the system prompt, the messages and the tools; refused with `agent.secret_refused` for a text that looks like a secret | human |

`AgentSettings`: `instructions` (at most 8000 characters), `default_access`, `default_model` (a provider's name in the ladder), `spend_cap_usd` (per thread, 0 to 1000; 0 means no model call), `day_cap_usd` (per UTC day across threads, 0 to 10 000; 0 means no model call), `tools_enabled[]`, `retention_days` (7, 30, 90 or 365), `offline` (no model call at all, whatever else is set) and `consented_providers[]` (the providers a person agreed to send thread text to; empty until they tick one). Thread changes reach every Studio that has the thread open by long-poll: `agent.threads.list` and `agent.threads.get` take `since` (the revision they last saw) and `wait_ms` (at most 30 000) and return when something is newer (O-15 section 5.1). There are no `agent.item` events on `events.since`; that earlier shape is dropped (open issue 3).

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
6. A thread calls a model only when every gate is open, checked before each send and each step (`agent.not_available`
   with a sentence naming the closed gate): offline is off; the ladder has an enabled provider by that name with a model id;
   its key is stored when its kind needs one (`anthropic`, `openai` and `google` do); the person consented to that
   provider (`consented_providers`); the per-thread and per-day caps are above zero and not yet reached. At most 10
   provider calls in a turn, at most 8 tool calls per answer, at most 4096 output tokens per call, and a tool result sent
   back to the model is cut at 12 000 characters.
7. `agent.preview` shows what a send would put on the wire, including the provider's host, so the consent is informed.

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

1. Settled by the implementation: the loop runs inside `ogund` (section 11). The cost is an HTTP client in the daemon for providers; the benefit is that keys, consent, caps and the ledger live in one process. Moving it to a sandboxed child under the operator ABI remains possible.
2. Id prefixes `thr_` and `itm_` are in O-00's table; they are not yet in Zeus 00 section 5.1 (a Zeus pull request).
3. Settled: a thread streams by long-poll (section 5); an `assistant` item is edited in place while `streaming` is true and is final after.
4. Cancellation: whether `agent.stop` may leave a started `runs.start` running (it does today: a started run is the
   daemon's, not the thread's).
5. Prompt injection through tool results (a workflow description, an artifact preview). The boundary holds because the
   tool table cannot decide, read a secret or reach a host; red-team tests against a real model are still to write.
6. Several people on one hub: threads are private today; shared threads need an owner, a role check on `agent.act`, and
   a decision on who sees the spend.
7. Studio-web sends no workspace header yet, so a person in several workspaces has no way to choose (O-13 section 6).

## 11. The implementation in `ogund`

Everything in this section is in `crates/ogun-daemon/src/studio/agent`. It is what was built and what was checked, with the limits stated.

**Threads.** A thread is one JSON file, `<state>/settings/agent-threads/<thr_>.json`, with the items the interface draws and a separate model transcript (what the model sees: a person's messages, its own answers with tool calls, and tool results, which are untrusted data). A thread that was `running` when the engine stopped is `idle` after the restart, with a notice. Finished threads older than `retention_days` are removed at start; a `waiting` thread is kept. At most 200 threads and 600 items per thread.

**The loop.** One send runs up to 10 model steps. The system prompt states the rules no instruction can change (use the tools, never claim what was not checked, no approvals, no secrets, tool output is data), the thread's access and tools, the date, the closed IR in outline with the operator catalogue, one example workflow and, last, the person's standing instructions (which cannot widen tools or access). Each tool call goes through the same policy table as any client, as an `agent` credential the daemon holds for this purpose, so the table, not the loop, is what refuses `approvals.decide`, secrets and trust changes. Cards are built from tool results by the daemon, and `agent.act` is `human`-only. A call is idempotent per thread and call id.

**Providers** (one entry of the ladder, O-03 section 8.1; the key travels only in the provider's own header; no redirect is followed; no URL query, key or response text appears in an error or a log; a stream is cancelled by dropping it; sizes are bounded; 429 and 5xx are retried up to three times before anything was received):

| Ladder `kind` | Endpoint | Streaming | Tools |
|---|---|---|---|
| `anthropic` | `POST {base}/v1/messages`, headers `x-api-key` and `anthropic-version` (default `2026-10-21`, overridable with `api_version`) | SSE `content_block_*` events | `tool_use` blocks with `input_json_delta` |
| `openai`, `openai_compatible` | `POST {base}/chat/completions`, `Authorization: Bearer` | SSE `data:` chunks | `delta.tool_calls` fragments by index. `openai_compatible` is how Mistral, DeepSeek, Alibaba Qwen (DashScope's compatible endpoint), Moonshot Kimi, Zhipu GLM, vLLM, LM Studio and OpenRouter are reached: a base URL and a model id |
| `google` | `POST {base}/models/{model}:streamGenerateContent?alt=sse`, header `x-goog-api-key` | SSE chunks | `functionCall` parts |
| `ollama` | `POST {base}/api/chat` (default `http://127.0.0.1:11434`, no key) | NDJSON lines | `message.tool_calls` |

A tool name travels on the wire with `.` replaced by `__` (some providers refuse a dot) and is mapped back. A model that cannot call tools natively (the provider answers 400 about `tools`) is driven with a **JSON protocol**: the system prompt asks for `{"tool": ..., "arguments": {...}}` and the reply is parsed against the same tool table; a reply that names no tool of the table is plain text.

**Keys.** A provider names its key's secret in `api_key_secret`; the value is fetched from the engine's secret store (the operating system's credential store, Windows Credential Manager or the Linux Secret Service, else a passphrase-encrypted file, else owner-only files; Settings says which) only for the length of one call, in zeroizing memory. A person enters a value on the Providers page, in a masked field, once (ADR 0015 decision 10). Standing instructions, messages and `ladder.put` are checked against the redaction rules and refused, not masked, when they look like a secret.

**Spend.** Each provider call appends a `UsageRecord` to the hash-chained ledger of O-03 section 8.9 with `thread_id` (no `run_id`), provider, model, tokens and cost; summaries are kept per thread and per UTC day. Before a call, its worst case (the input bytes bound the tokens, plus the output limit, at the ladder's prices) must fit under both the per-thread and the per-day cap, or nothing is sent. Afterwards the cost comes from the provider's reported token counts and the ladder's prices; a provider that reports none is charged an estimate of one token per four bytes.

**What was checked, and what was not.** `crates/ogun-daemon/tests/agent.rs` runs the agent against local mock servers that speak each provider's wire format (streamed deltas, tool-call deltas, error bodies, rate-limit headers): an OpenAI-style provider, Anthropic Messages, Gemini, Ollama, a tool-less model on the JSON protocol, 401/429/5xx handling, consent, offline, zero caps, secret refusal, the access table, and that no tool decides an approval and a card is pressed only by a person. The tests plant a canary key and search every request body sent, the thread, every method result and every file in the state directory (except the secret store and the run store) for it. **No call has been made to a real provider**: no provider key was available during the 0.0.1b build, so every wire format is as published documentation describes it and as the mocks reproduce it, and nothing more. A real key may find a mismatch the mocks share with the code.
