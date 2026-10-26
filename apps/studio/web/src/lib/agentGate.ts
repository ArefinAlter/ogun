// Whether the agent can answer, and if not, why, in words a person can act on. The daemon is the authority (it
// refuses a call that breaks any of these); this only lets the screen say so before a person writes a message.
// Nothing is sent anywhere until one provider has a model, a stored key, the person's consent, the offline switch
// is off and both spend caps are above zero.

import type { AgentSettings, LadderProvider } from '../api/types'

export type GateReason = 'offline' | 'no_cap' | 'no_provider' | 'no_key' | 'no_consent'

export interface Readiness {
  ready: boolean
  reason?: GateReason
  /** One plain sentence for the banner. */
  message: string
  /** The ladder entry that would answer, when ready. */
  provider?: string
}

const NEEDS_KEY = new Set(['anthropic', 'openai', 'google'])

export function agentReadiness(input: {
  settings: Pick<AgentSettings, 'offline' | 'consented_providers' | 'spend_cap_usd' | 'day_cap_usd'> | null | undefined
  providers: Pick<LadderProvider, 'name' | 'kind' | 'enabled' | 'model' | 'api_key_secret' | 'capabilities'>[] | null | undefined
  secretNames: string[]
}): Readiness {
  const { settings, providers } = input
  // Without settings or a ladder to read there is nothing to judge: leave the decision to the daemon.
  if (!settings || !providers) return { ready: true, message: '' }
  if (settings.offline) return { ready: false, reason: 'offline', message: 'Offline is on: no model call is made. Turn it off in Settings, Providers to use the agent.' }
  if (!(settings.spend_cap_usd > 0) || !(settings.day_cap_usd > 0)) {
    return { ready: false, reason: 'no_cap', message: 'A spend cap is 0, and a cap of 0 means no model call. Raise it in Settings, Providers.' }
  }
  const usable = providers.filter((p) => p.enabled && p.model.trim() !== '' && p.capabilities.includes('chat'))
  if (usable.length === 0) return { ready: false, reason: 'no_provider', message: 'No provider is configured. Add one in Settings, Providers, with its model.' }
  const stored = new Set(input.secretNames)
  const withKey = usable.filter((p) => (p.api_key_secret ? stored.has(p.api_key_secret) : !NEEDS_KEY.has(p.kind)))
  if (withKey.length === 0) return { ready: false, reason: 'no_key', message: 'A provider is set up but has no key stored. Add a provider key in Settings, Providers.' }
  const allowed = withKey.find((p) => settings.consented_providers.includes(p.name))
  if (!allowed) return { ready: false, reason: 'no_consent', message: 'Nothing is sent to a provider until you allow it. Tick "Allow the agent to send prompts" for one provider in Settings, Providers.' }
  return { ready: true, message: '', provider: allowed.name }
}
