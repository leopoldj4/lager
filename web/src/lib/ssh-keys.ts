import { rest } from "./rest"

export interface SshKeyInfo {
  id: number
  name: string
  fingerprint: string
  algorithm: string
  created_ms: number
  last_used_ms: number | null
}

export function listSshKeys(): Promise<{ keys: SshKeyInfo[] }> {
  return rest("/ssh-keys", "SSH key request")
}

export function addSshKey(key: {
  key: string
  name?: string
}): Promise<{ key: SshKeyInfo }> {
  return rest("/ssh-keys", "SSH key request", "POST", key)
}

export function removeSshKey(id: number) {
  return rest(`/ssh-keys/${id}`, "SSH key request", "DELETE")
}
