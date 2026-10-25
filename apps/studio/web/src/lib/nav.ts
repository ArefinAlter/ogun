// The sidebar's sections. Kept apart from the shell so the command palette can list them without importing it.

import type { ShellInfo } from '../api'
import type { IconName } from '../ui/icons'

export interface NavItem {
  id: string
  label: string
  icon: IconName
  group: string
  /** The hash path when it is not just `[id]`. */
  to?: string[]
  show?: (i: ShellInfo | null) => boolean
}

export const NAV: NavItem[] = [
  { id: 'agent', label: 'Agent', icon: 'chat', group: 'Operate', show: (i) => !!i?.agent },
  { id: '', label: 'Overview', icon: 'gauge', group: 'Operate' },
  { id: 'workflows', label: 'Workflows', icon: 'dag', group: 'Operate' },
  { id: 'runs', label: 'Runs', icon: 'play', group: 'Operate' },
  { id: 'partitions', label: 'Partitions', icon: 'matrix', group: 'Operate' },
  { id: 'approvals', label: 'Approvals', icon: 'seal', group: 'Trust' },
  { id: 'artifacts', label: 'Artifacts and lineage', icon: 'cube', group: 'Data' },
  { id: 'streams', label: 'Streams', icon: 'wave', group: 'Data' },
  { id: 'edges', label: 'Edges', icon: 'compass', group: 'Data' },
  { id: 'connectors', label: 'Connectors', icon: 'plug', group: 'Data', to: ['settings', 'connectors'] },
  { id: 'gateway', label: 'Field gateway', icon: 'anvil', group: 'Data', show: (i) => !!i?.gateway },
]
