// Model provider presets for the Providers page. Each becomes one entry of the ladder file `<state_dir>/llm.json`
// (O-03 section 8.1). Models are optional in Ogun: a workflow without an `llm.*` operator runs with no model
// access, and a missing or empty ladder means no model is ever called.
//
// The base URLs are the vendors' documented OpenAI-compatible or native endpoints, written from public
// documentation and not called from here. Model names change faster than this file does, so every entry is
// written disabled with an empty `model` (which the ladder allows for a disabled entry): a person fills it in.

import type { Region } from './types'

export type LadderKind = 'anthropic' | 'openai' | 'openai_compatible' | 'ollama' | 'google'

export interface ProviderPreset {
  id: string
  name: string
  vendor: string
  kind: LadderKind
  base_url: string
  regions: Region[]
  /** A name that suits the ladder's `name` pattern. */
  entry_name: string
  /** The secret to hold the key, or null for a local server. */
  secret: string | null
  note?: string
  docs_url: string
}

export const PROVIDERS: ProviderPreset[] = [
  { id: 'anthropic', name: 'Anthropic', vendor: 'Anthropic', kind: 'anthropic', base_url: 'https://api.anthropic.com', regions: ['global'], entry_name: 'anthropic', secret: 'anthropic_api_key', docs_url: 'https://docs.anthropic.com' },
  { id: 'openai', name: 'OpenAI', vendor: 'OpenAI', kind: 'openai', base_url: 'https://api.openai.com/v1', regions: ['global'], entry_name: 'openai', secret: 'openai_api_key', docs_url: 'https://platform.openai.com/docs' },
  { id: 'google', name: 'Google Gemini', vendor: 'Google', kind: 'google', base_url: 'https://generativelanguage.googleapis.com/v1beta', regions: ['global'], entry_name: 'gemini', secret: 'google_api_key', docs_url: 'https://ai.google.dev/gemini-api/docs' },
  { id: 'ollama', name: 'Ollama (local)', vendor: 'Ollama', kind: 'ollama', base_url: 'http://127.0.0.1:11434', regions: ['global'], entry_name: 'ollama', secret: null, note: 'Runs on this machine; no key and no egress.', docs_url: 'https://ollama.com' },
  { id: 'vllm', name: 'vLLM or llama.cpp server (local)', vendor: 'Self-hosted', kind: 'openai_compatible', base_url: 'http://127.0.0.1:8000/v1', regions: ['global', 'cn'], entry_name: 'local_openai', secret: null, note: 'Any server that speaks the OpenAI chat shape. Plain http is allowed to a loopback host only.', docs_url: 'https://docs.vllm.ai' },
  { id: 'openai-compatible', name: 'OpenAI-compatible base URL', vendor: 'Your own', kind: 'openai_compatible', base_url: 'https://', regions: ['global', 'cn'], entry_name: 'custom', secret: 'custom_api_key', note: 'Any server that speaks the OpenAI chat shape: put its base URL in the next step.', docs_url: 'https://platform.openai.com/docs/api-reference/chat' },
  { id: 'lm-studio', name: 'LM Studio (local)', vendor: 'LM Studio', kind: 'openai_compatible', base_url: 'http://127.0.0.1:1234/v1', regions: ['global'], entry_name: 'lm_studio', secret: null, docs_url: 'https://lmstudio.ai/docs' },
  { id: 'openrouter', name: 'OpenRouter', vendor: 'OpenRouter', kind: 'openai_compatible', base_url: 'https://openrouter.ai/api/v1', regions: ['global'], entry_name: 'openrouter', secret: 'openrouter_api_key', docs_url: 'https://openrouter.ai/docs' },
  { id: 'mistral', name: 'Mistral', vendor: 'Mistral AI', kind: 'openai_compatible', base_url: 'https://api.mistral.ai/v1', regions: ['global'], entry_name: 'mistral', secret: 'mistral_api_key', docs_url: 'https://docs.mistral.ai' },
  { id: 'groq', name: 'Groq', vendor: 'Groq', kind: 'openai_compatible', base_url: 'https://api.groq.com/openai/v1', regions: ['global'], entry_name: 'groq', secret: 'groq_api_key', docs_url: 'https://console.groq.com/docs' },
  { id: 'together', name: 'Together AI', vendor: 'Together AI', kind: 'openai_compatible', base_url: 'https://api.together.xyz/v1', regions: ['global'], entry_name: 'together', secret: 'together_api_key', docs_url: 'https://docs.together.ai' },
  { id: 'deepseek', name: 'DeepSeek', vendor: 'DeepSeek', kind: 'openai_compatible', base_url: 'https://api.deepseek.com', regions: ['cn', 'global'], entry_name: 'deepseek', secret: 'deepseek_api_key', docs_url: 'https://api-docs.deepseek.com' },
  { id: 'qwen-cn', name: 'Qwen on Alibaba Model Studio (China)', vendor: 'Alibaba Cloud', kind: 'openai_compatible', base_url: 'https://dashscope.aliyuncs.com/compatible-mode/v1', regions: ['cn'], entry_name: 'qwen_cn', secret: 'dashscope_api_key', note: 'OpenAI-compatible mode of DashScope. Keys are per region.', docs_url: 'https://www.alibabacloud.com/help/en/model-studio/' },
  { id: 'qwen-intl', name: 'Qwen on Alibaba Model Studio (international)', vendor: 'Alibaba Cloud', kind: 'openai_compatible', base_url: 'https://dashscope-intl.aliyuncs.com/compatible-mode/v1', regions: ['global'], entry_name: 'qwen_intl', secret: 'dashscope_intl_api_key', docs_url: 'https://www.alibabacloud.com/help/en/model-studio/' },
  { id: 'moonshot-cn', name: 'Kimi on Moonshot AI (China)', vendor: 'Moonshot AI', kind: 'openai_compatible', base_url: 'https://api.moonshot.cn/v1', regions: ['cn'], entry_name: 'kimi_cn', secret: 'moonshot_api_key', docs_url: 'https://platform.moonshot.cn/docs' },
  { id: 'moonshot-intl', name: 'Kimi on Moonshot AI (international)', vendor: 'Moonshot AI', kind: 'openai_compatible', base_url: 'https://api.moonshot.ai/v1', regions: ['global'], entry_name: 'kimi_intl', secret: 'moonshot_intl_api_key', docs_url: 'https://platform.moonshot.ai/docs' },
  { id: 'zhipu', name: 'GLM on Zhipu AI (China)', vendor: 'Zhipu AI', kind: 'openai_compatible', base_url: 'https://open.bigmodel.cn/api/paas/v4', regions: ['cn'], entry_name: 'glm', secret: 'zhipu_api_key', docs_url: 'https://open.bigmodel.cn/dev/api' },
  { id: 'volcengine-ark', name: 'Doubao on Volcano Engine Ark', vendor: 'Volcano Engine', kind: 'openai_compatible', base_url: 'https://ark.cn-beijing.volces.com/api/v3', regions: ['cn'], entry_name: 'doubao', secret: 'ark_api_key', note: 'The model field is an endpoint or model id from the Ark console.', docs_url: 'https://www.volcengine.com/docs/82379' },
  { id: 'baidu-qianfan', name: 'ERNIE on Baidu Qianfan', vendor: 'Baidu', kind: 'openai_compatible', base_url: 'https://qianfan.baidubce.com/v2', regions: ['cn'], entry_name: 'ernie', secret: 'qianfan_api_key', docs_url: 'https://cloud.baidu.com/doc/qianfan/index.html' },
  { id: 'tencent-hunyuan', name: 'Hunyuan on Tencent Cloud', vendor: 'Tencent Cloud', kind: 'openai_compatible', base_url: 'https://api.hunyuan.cloud.tencent.com/v1', regions: ['cn'], entry_name: 'hunyuan', secret: 'hunyuan_api_key', docs_url: 'https://cloud.tencent.com/document/product/1729' },
  { id: 'siliconflow', name: 'SiliconFlow', vendor: 'SiliconFlow', kind: 'openai_compatible', base_url: 'https://api.siliconflow.cn/v1', regions: ['cn'], entry_name: 'siliconflow', secret: 'siliconflow_api_key', docs_url: 'https://docs.siliconflow.cn' },
]

/** One ladder entry, as JSON text, ready for `llm.json`. Disabled, with an empty model, as the ladder allows. */
export function ladderEntry(p: ProviderPreset): string {
  const entry: Record<string, unknown> = {
    name: p.entry_name,
    kind: p.kind,
    enabled: false,
    base_url: p.base_url,
    model: '',
  }
  if (p.secret) entry.api_key_secret = p.secret
  entry.input_per_mtok_usd_micro = 0
  entry.output_per_mtok_usd_micro = 0
  entry.max_output_tokens = 8192
  return JSON.stringify(entry, null, 2)
}

/** The hosts a preset contacts, for the egress approval. */
export function providerHost(p: ProviderPreset): string {
  try {
    return new URL(p.base_url).host
  } catch {
    return p.base_url
  }
}
