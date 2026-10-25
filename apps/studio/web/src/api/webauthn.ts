// WebAuthn for Studio-web approvals (O-15 section 7). The server returns PublicKeyCredentialRequestOptions whose
// challenge is base64url(BLAKE3(approval_id || request_hash || decision)); the daemon verifies the assertion itself.

export interface AssertionJson {
  credential_id: string
  client_data_json: string
  authenticator_data: string
  signature: string
}

export function b64urlToBytes(s: string): Uint8Array<ArrayBuffer> {
  const pad = s.length % 4 === 0 ? '' : '='.repeat(4 - (s.length % 4))
  const bin = atob(s.replace(/-/g, '+').replace(/_/g, '/') + pad)
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}

export function bytesToB64url(b: ArrayBuffer | Uint8Array): string {
  const bytes = b instanceof Uint8Array ? b : new Uint8Array(b)
  let bin = ''
  for (const x of bytes) bin += String.fromCharCode(x)
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

interface RequestOptionsJson {
  challenge: string
  rpId?: string
  timeout?: number
  userVerification?: UserVerificationRequirement
  allowCredentials?: { id: string; type: 'public-key'; transports?: AuthenticatorTransport[] }[]
}

/** Accepts the options either bare or wrapped as `{publicKey: ...}`. */
export function toRequestOptions(json: unknown): PublicKeyCredentialRequestOptions {
  const raw = (json && typeof json === 'object' && 'publicKey' in json
    ? (json as { publicKey: unknown }).publicKey
    : json) as RequestOptionsJson
  if (!raw || typeof raw.challenge !== 'string') {
    throw new Error('the server did not return WebAuthn request options')
  }
  return {
    challenge: b64urlToBytes(raw.challenge),
    ...(raw.rpId ? { rpId: raw.rpId } : {}),
    ...(raw.timeout ? { timeout: raw.timeout } : {}),
    // The daemon requires the user-verified flag.
    userVerification: 'required',
    ...(raw.allowCredentials
      ? {
          allowCredentials: raw.allowCredentials.map((c) => ({
            type: 'public-key' as const,
            id: b64urlToBytes(c.id),
            ...(c.transports ? { transports: c.transports } : {}),
          })),
        }
      : {}),
  }
}

export function webauthnAvailable(): boolean {
  return typeof window !== 'undefined' && 'PublicKeyCredential' in window && !!navigator.credentials
}

/** Asks the person's authenticator for an assertion over the server's challenge. */
export async function getAssertion(options: PublicKeyCredentialRequestOptions): Promise<AssertionJson> {
  const cred = (await navigator.credentials.get({ publicKey: options })) as PublicKeyCredential | null
  if (!cred) throw new Error('no passkey was used')
  const r = cred.response as AuthenticatorAssertionResponse
  return {
    credential_id: bytesToB64url(cred.rawId),
    client_data_json: bytesToB64url(r.clientDataJSON),
    authenticator_data: bytesToB64url(r.authenticatorData),
    signature: bytesToB64url(r.signature),
  }
}
