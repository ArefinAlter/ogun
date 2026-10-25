// Settings: one page per concern, in the order of T3 Code's settings, mapped to what Ogun has. The shell shows the
// section list in place of the sidebar (see app.tsx); this file owns the list and the routing.

import type { IconName } from '../../ui/icons'
import { href } from '../../lib/hooks'
import { Empty } from '../../ui/components'
import { AgentSettingsPage } from './Agent'
import { AppearanceSettings } from './Appearance'
import { ConnectorDetail, ConnectorsCatalog } from './Connectors'
import { ConnectionEditor, ConnectionsList } from './Connections'
import { EnvironmentsSettings } from './Environments'
import { EvidenceSettingsPage } from './Evidence'
import { GeneralSettings } from './General'
import { KeybindingSettings } from './Keybindings'
import { ProvidersSettings } from './Providers'
import { SchedulesSettings } from './Schedules'
import { SecuritySettings } from './Security'
import { SourcesSettings } from './Sources'
import { ArchiveSettings, StorageSettings } from './StorageArchive'

export interface SettingsSection {
  id: string
  label: string
  icon: IconName
  /** One line for the command palette. */
  hint: string
}

export const SETTINGS_SECTIONS: SettingsSection[] = [
  { id: 'general', label: 'General', icon: 'sliders', hint: 'Run defaults, per-workflow overrides, device behaviour' },
  { id: 'appearance', label: 'Appearance', icon: 'palette', hint: 'Color scheme, themes, contrast, density' },
  { id: 'keybindings', label: 'Keybindings', icon: 'keyboard', hint: 'Shortcuts and when they fire' },
  { id: 'evidence', label: 'Evidence', icon: 'ledger', hint: 'Per-run evidence bundles' },
  { id: 'providers', label: 'Providers', icon: 'sparkle', hint: 'The model ladder and its spend' },
  { id: 'agent', label: 'Agent', icon: 'chat', hint: 'Instructions, access and limits for agent threads' },
  { id: 'connectors', label: 'Connectors', icon: 'plug', hint: 'Warehouses, storage, streams, observability, chat' },
  { id: 'connections', label: 'Connections', icon: 'link', hint: 'Configured uses of a connector' },
  { id: 'schedules', label: 'Schedules', icon: 'clock', hint: 'Cron triggers and streams' },
  { id: 'sources', label: 'Workflow sources', icon: 'dag', hint: 'Folders and repositories of workflows' },
  { id: 'storage', label: 'Storage', icon: 'drawers', hint: 'The artifact store and cleanup rules' },
  { id: 'environments', label: 'Environments', icon: 'globe', hint: 'Daemon, hubs, edges and bridges' },
  { id: 'security', label: 'Security', icon: 'shield', hint: 'Credentials and secret names' },
  { id: 'archive', label: 'Archive', icon: 'archive', hint: 'Set-aside versions and runs' },
]

export function Settings({ path, query }: { path: string[]; query: URLSearchParams }) {
  const section = path[1] ?? 'general'
  const id = path[2]
  switch (section) {
    case 'general':
      return <GeneralSettings />
    case 'appearance':
      return <AppearanceSettings />
    case 'keybindings':
      return <KeybindingSettings />
    case 'evidence':
      return <EvidenceSettingsPage />
    case 'providers':
      return <ProvidersSettings />
    case 'agent':
      return <AgentSettingsPage />
    case 'connectors':
      return id ? <ConnectorDetail id={id} /> : <ConnectorsCatalog />
    case 'connections':
      return id ? <ConnectionEditor name={id} {...(query.get('connector') ? { connectorId: query.get('connector') as string } : {})} /> : <ConnectionsList />
    case 'schedules':
      return <SchedulesSettings />
    case 'sources':
      return <SourcesSettings />
    case 'storage':
      return <StorageSettings />
    case 'environments':
      return <EnvironmentsSettings />
    case 'security':
      return <SecuritySettings />
    case 'archive':
      return <ArchiveSettings />
    default:
      return (
        <Empty>
          No such settings page. <a href={href('settings')}>General</a>
        </Empty>
      )
  }
}
