// The agent's tools and what each access level allows (O-17, proposed). Plain data with no imports of its own, so the
// rules can be tested with `node --test` and read by the interface, the scripted agent and, later, the daemon.
//
// The set is closed and mirrors what `ogun mcp` and the hub's MCP route already expose, plus validation and
// planning. What is not in it matters as much: there is no tool that decides an approval, reads a secret value,
// runs a shell command or reaches a host, so no access level and no instruction text can add one.

import type { AgentAccess, AgentToolInfo } from '../api/types'

export const ACCESS_ORDER: AgentAccess[] = ['read', 'author', 'operate']

export const ACCESS_INFO: Record<AgentAccess, { label: string; blurb: string }> = {
  read: { label: 'Read only', blurb: 'Reads workflows, runs, partitions, artifacts, lineage and approvals; validates and plans. Changes nothing.' },
  author: { label: 'Author', blurb: 'Also stores workflow versions. A version that changes code still waits for a person.' },
  operate: { label: 'Operate', blurb: 'Also starts and cancels runs. Egress, code and budget approvals still wait for a person.' },
}

export const TOOLS: AgentToolInfo[] = [
  { name: 'status.get', access: 'read', summary: 'Counts of workflows, active runs, pending approvals and streams' },
  { name: 'workflows.list', access: 'read', summary: 'Stored workflows and their latest versions' },
  { name: 'workflows.get', access: 'read', summary: 'One workflow document with its hash' },
  { name: 'workflows.validate', access: 'read', summary: 'Checks a workflow document without storing it' },
  { name: 'runs.list', access: 'read', summary: 'Recent runs, newest first' },
  { name: 'runs.get', access: 'read', summary: 'One run and its node instances' },
  { name: 'partitions.status', access: 'read', summary: 'Which partitions are done, running, failed or missing' },
  { name: 'backfills.plan', access: 'read', summary: 'What a backfill over a range would run' },
  { name: 'artifacts.list', access: 'read', summary: 'Artifacts a run produced' },
  { name: 'artifacts.read', access: 'read', summary: 'A bounded preview of one artifact' },
  { name: 'lineage.get', access: 'read', summary: 'OpenLineage events of a run or an artifact' },
  { name: 'approvals.list', access: 'read', summary: 'Approval requests and their facts (read only)' },
  { name: 'connectors.search', access: 'read', summary: 'The connector catalogue: what each system needs' },
  { name: 'workflows.put', access: 'author', summary: 'Stores a validated workflow as a new version' },
  { name: 'runs.start', access: 'operate', summary: 'Starts a run or a backfill (idempotent)' },
  { name: 'runs.cancel', access: 'operate', summary: 'Cancels a run' },
]

/** What no tool does, whatever the access level or the instructions say. Shown in the interface as fixed rules. */
export const NEVER: string[] = [
  'Decide an approval. Only a person does, on a terminal or in Studio.',
  'Read a secret value. It sees secret names, never what they hold.',
  'Run a shell command, or any code that is not a stored, approved operator.',
  'Reach a host that has no egress approval.',
]

export function accessRank(a: AgentAccess): number {
  return ACCESS_ORDER.indexOf(a)
}

/** Whether a thread at `access` may call `tool`, given the tools the settings allow. */
export function mayCall(tool: string, access: AgentAccess, enabled: readonly string[]): { ok: true } | { ok: false; reason: string } {
  const t = TOOLS.find((x) => x.name === tool)
  if (!t) return { ok: false, reason: 'there is no such tool' }
  if (!enabled.includes(tool)) return { ok: false, reason: 'it is switched off in Settings, Agent' }
  if (accessRank(access) < accessRank(t.access)) return { ok: false, reason: `it needs ${ACCESS_INFO[t.access].label} access and this thread has ${ACCESS_INFO[access].label}` }
  return { ok: true }
}

export const DEFAULT_INSTRUCTIONS = [
  'You are working on this team\'s Ogun daemon.',
  '- Prefer the stored workflows over new ones; say which one you used.',
  '- Never put a secret, a token or a URL query string in a message or a workflow. Name a secret, do not show one.',
  '- A change to code needs a person\'s approval. Say so before proposing it.',
  '- Report what you checked and what you did not.',
].join('\n')
