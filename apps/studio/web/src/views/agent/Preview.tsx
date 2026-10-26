// "What will be sent": exactly what the next model request would carry, as the engine would build it (redacted),
// shown before anything leaves the computer. The agent's provider only receives this after the person has
// allowed that provider (Settings, Providers) and sends a message.

import { useBackend, useAsync } from '../../lib/hooks'
import { Chip, Dialog, ErrorBox, Loading, Notice } from '../../ui/components'

export function PreviewDialog({ threadId, text, onClose }: { threadId: string | null; text: string; onClose: () => void }) {
  const backend = useBackend()
  const preview = useAsync(() => (backend.agent?.preview ? backend.agent.preview(threadId, text) : Promise.reject(new Error('this engine cannot preview'))), [backend, threadId, text])
  const p = preview.data
  return (
    <Dialog title="What will be sent" onClose={onClose}>
      <p class="muted">The next request, exactly as the engine would build it. Keys are never in it: the engine adds the key to the request header itself, and values that look like secrets are refused, not masked.</p>
      <ErrorBox error={preview.error} title="Preview" />
      {preview.loading && !p ? <Loading /> : null}
      {p ? (
        <>
          <dl class="kvs">
            <dt>provider</dt>
            <dd>
              <span class="mono">{p.provider}</span>
            </dd>
            <dt>sent to</dt>
            <dd>
              <span class="mono">{p.host}</span>
            </dd>
            <dt>model</dt>
            <dd>
              <span class="mono">{p.model}</span>
            </dd>
          </dl>
          <div class="preview-block">
            <div class="role">system</div>
            <pre class="code-block mono">{p.system}</pre>
          </div>
          {p.messages.map((m, i) => (
            <div class="preview-block" key={i}>
              <div class="role">{m.role}</div>
              <pre class="code-block mono">{m.content}</pre>
            </div>
          ))}
          <div class="preview-block">
            <div class="role">tools the model may ask for ({p.tools.length})</div>
            <div>
              {p.tools.map((t) => (
                <Chip key={t}>{t}</Chip>
              ))}
            </div>
          </div>
          <Notice>Nothing has been sent. The model's answer is untrusted text: the tools it asks for run under this thread's access, and it can never decide an approval.</Notice>
        </>
      ) : null}
      <div class="toolbar">
        <button type="button" class="btn" onClick={onClose}>
          Close
        </button>
      </div>
    </Dialog>
  )
}
