# ADR 0014: Agent threads are daemon objects over a closed tool set; the agent proposes and a person presses

| | |
|---|---|
| Status | Proposed |
| Date | 2026-10-25 |
| Decided by | Pending the maintainer |
| Refines | ADR 0010 (server mode) and ADR 0011 (extension surfaces) |
| Specifies | O-17 |

## Context

Ogun is for data engineering, and the natural way to ask for data engineering is a conversation: "backfill the trips
workflow since 2019", "why did last night's forecast fail", "draft a daily workflow that lists trips per station". Studio had
no such screen. An agent could already reach the daemon from outside through MCP (`ogun mcp`, the hub's `/mcp` route,
ADR 0011), with the closed tool set that route exposes, but nothing inside Studio let a person talk to one, see what
it called, or press the button on what it proposed.

A conversation screen is also the place where Ogun's rules are most likely to be eroded by accident: a model's words
are untrusted text, a convenient "approve" button in a chat would put a decision where nobody read the facts, and a
provider key in a browser would put a secret where Ogun promises none.

## Decision

1. **A thread is a daemon object.** `ogund` holds threads, runs the agent loop and keeps the spend ledger, because it
   already holds the provider ladder (O-03 section 8.1), the secrets and the artifact store. Studio, on every shell, is a
   view of a thread. A browser tab never holds a provider key and never calls a model.
2. **The agent acts through a closed tool table.** The tools mirror what MCP exposes plus validation and planning:
   read tools, `workflows.put` (Author), `runs.start` and `runs.cancel` (Operate). The table is data (O-17 section 4),
   so Studio, the daemon and a test can read the same list. There is no tool that decides an approval, reads a secret
   value, runs a shell command or reaches a host. No instruction, access level or setting adds one.
3. **Access is per thread and only narrows.** A thread has `read`, `author` or `operate`; the effective set is the
   thread's access, intersected with the tools the settings allow and with the caller's own role. Raising access is a
   person's act in the interface; the agent cannot raise its own.
4. **The agent's words are untrusted; consequences are cards.** Prose is rendered as text, never as markup. Anything with
   consequences (a proposed workflow, a backfill plan, a retry, a pending approval) is a card the daemon builds from a
   tool result, and every action on a card is a button that a person presses, executed with that person's role and an
   idempotency key. A card for an approval links to the Approvals page; it has no decide button, ever.
5. **Models are optional and spend is capped.** A thread needs a ladder entry. Spend counts in the same ledger as the
   `llm.*` operators, with a per-thread cap; a cap of zero means no model call. A daemon without a ladder offers no
   threads and every workflow still runs (constraint 6).
6. **Secrets stay out of the model.** Standing instructions and every message are checked against the redaction rules
   before they reach a prompt; a value that looks like a secret is refused, not masked. The agent sees secret names only.
7. **Studio is honest when the daemon has no threads.** It shows the page as unavailable and gives the MCP commands that
   work today, instead of a chat box that cannot answer.

## Consequences

- The daemon API grows (`agent.*`, O-17) and the closed O-15 set has to admit them by an accepted change; until then
  Studio feature-detects and the scripted demo implements them for demonstration only.
- Threads are stored data: a thread holds a person's questions and the daemon's answers. They get a retention setting and
  are never exported by the exporters of ADR 0013.
- The agent loop is a new process that talks to a model, so it needs the sandbox and egress rules of an operator; where
  exactly it runs (in `ogund`, or as a child under the operator ABI) is open issue 1 of O-17.
- A prompt-injection that arrives in a tool result (a workflow description, an artifact preview) cannot do more than the
  thread's access allows, and cannot decide an approval, which is the property that makes the screen safe to ship.

## Alternatives considered

- **A model client in Studio:** rejected. It would put a provider key in the webview and keep spend and history on one
  device. Constraint 4 is easier to keep when no screen holds a key.
- **MCP only, no screen:** kept as the route that works today, and shown in Studio, but it leaves the person with no view
  of what the agent called and no place to press what it proposes.
- **An `llm` operator that answers chat:** rejected. A chat is not a workflow node: it has no partitions, no determinism
  class and no run record to hash.
- **Approve and deny buttons in the thread:** rejected without a trade-off. The facts-first approval surface and its
  arming delay exist because a decision made in a conversation is a decision nobody read (ADR 0010 point 3).
