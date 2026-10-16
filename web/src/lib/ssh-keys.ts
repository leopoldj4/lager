export interface SshKeyInfo {
  id: number
  name: string
  fingerprint: string
  algorithm: string
  created_ms: number
  last_used_ms: number | null
}

async function request(path: string, init?: RequestInit) {
  const response = await fetch(`/-/api/ssh-keys${path}`, {
    ...init,
    headers: { "content-type": "application/json", ...init?.headers },
  })
  if (!response.ok) {
    const error = await response.json().catch(() => ({}))
    throw new Error(
      error.message ?? `SSH key request failed (${response.status})`
    )
  }
  if (response.status === 204) return
  return response.json()
}

export function listSshKeys(): Promise<{ keys: SshKeyInfo[] }> {
  return request("")
}

export function addSshKey(key: {
  key: string
  name?: string
}): Promise<{ key: SshKeyInfo }> {
  return request("", { method: "POST", body: JSON.stringify(key) })
}

export function removeSshKey(id: number) {
  return request(`/${id}`, { method: "DELETE" })
}
