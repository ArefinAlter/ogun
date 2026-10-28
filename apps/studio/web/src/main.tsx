import { render } from 'preact'

import { pickBackend } from './api'
import { App } from './app'
import { getPrefs } from './lib/prefs'
import { applyPrefs } from './lib/themes'
import './styles/tokens.css'
import './styles/app.css'
import './styles/settings.css'
import './styles/chrome.css'
import './styles/agent.css'
import './styles/layout.css'
import './styles/engine.css'
import './styles/bench.css'

const root = document.getElementById('app')
if (root) {
  applyPrefs(getPrefs())
  const backend = await pickBackend()
  render(<App backend={backend} />, root)
}
