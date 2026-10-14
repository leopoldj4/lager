import { live } from "./live"
import { queryClient } from "./api"

export interface PasskeyInfo {
  id: string
  name: string
}

export function supportsPasskeys() {
  return (
    typeof window !== "undefined" &&
    window.isSecureContext &&
    typeof PublicKeyCredential !== "undefined"
  )
}

async function request(path: string, body?: unknown, method = "POST") {
  const response = await fetch(`/-/api/passkeys${path}`, {
    method,
    headers: { "content-type": "application/json" },
    ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
  })
  if (!response.ok) {
    const error = await response.json().catch(() => ({}))
    throw new Error(
      error.message ?? `Passkey request failed (${response.status})`
    )
  }
  if (response.status === 204 || response.status === 201) return
  return response.json()
}

function decode(value: string): ArrayBuffer {
  const raw = atob(value.replace(/-/g, "+").replace(/_/g, "/"))
  return Uint8Array.from(raw, (c) => c.charCodeAt(0)).buffer
}

function encode(value: ArrayBuffer): string {
  return btoa(String.fromCharCode(...new Uint8Array(value)))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "")
}

type Descriptor = Omit<PublicKeyCredentialDescriptor, "id"> & { id: string }
function descriptors(values?: Descriptor[]): PublicKeyCredentialDescriptor[] {
  return (values ?? []).map((v) => ({ ...v, id: decode(v.id) }))
}

function serialize(credential: PublicKeyCredential) {
  const response = credential.response
  return {
    id: credential.id,
    rawId: encode(credential.rawId),
    type: credential.type,
    extensions: credential.getClientExtensionResults(),
    response:
      response instanceof AuthenticatorAttestationResponse
        ? {
            clientDataJSON: encode(response.clientDataJSON),
            attestationObject: encode(response.attestationObject),
            transports: response.getTransports?.() ?? [],
          }
        : {
            clientDataJSON: encode(response.clientDataJSON),
            authenticatorData: encode(
              (response as AuthenticatorAssertionResponse).authenticatorData
            ),
            signature: encode(
              (response as AuthenticatorAssertionResponse).signature
            ),
            userHandle: (response as AuthenticatorAssertionResponse).userHandle
              ? encode((response as AuthenticatorAssertionResponse).userHandle!)
              : null,
          },
  }
}

function requireSupport() {
  if (!supportsPasskeys()) {
    throw new Error(
      "Passkeys need a supported browser and HTTPS (or localhost)."
    )
  }
}

export function passkeyError(error: unknown) {
  if (error instanceof DOMException && error.name === "NotAllowedError") {
    return "Passkey request was canceled or timed out. You can try again."
  }
  if (error instanceof DOMException && error.name === "InvalidStateError") {
    return "This passkey is already registered. Try another authenticator."
  }
  return error instanceof Error ? error.message : "Passkey request failed."
}

export async function signInWithPasskey() {
  requireSupport()
  const { publicKey } = await request("/login/start")
  const credential = (await navigator.credentials.get({
    publicKey: {
      ...publicKey,
      challenge: decode(publicKey.challenge),
      allowCredentials: descriptors(publicKey.allowCredentials),
    },
  })) as PublicKeyCredential | null
  if (!credential) throw new Error("No passkey was selected.")
  await request("/login/finish", serialize(credential))
  live.reconnect()
  queryClient.clear()
}

export async function addPasskey(name: string) {
  requireSupport()
  const { publicKey } = await request("/register/start", { name })
  const credential = (await navigator.credentials.create({
    publicKey: {
      ...publicKey,
      challenge: decode(publicKey.challenge),
      user: { ...publicKey.user, id: decode(publicKey.user.id) },
      excludeCredentials: descriptors(publicKey.excludeCredentials),
    },
  })) as PublicKeyCredential | null
  if (!credential) throw new Error("No passkey was created.")
  await request("/register/finish", serialize(credential))
}

export function listPasskeys(): Promise<{
  enabled: boolean
  passkeys: PasskeyInfo[]
}> {
  return request("", undefined, "GET")
}

export function removePasskey(id: string) {
  return request(`/${encodeURIComponent(id)}`, undefined, "DELETE")
}
