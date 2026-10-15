export type TokenAccess = "none" | "read" | "write" | "force"

export interface TokenInfo {
  id: number
  name: string
  access: Record<string, TokenAccess> | null
  created_ms: number
  expires_ms: number | null
  last_used_ms: number | null
}

export interface NewToken {
  name: string
  access?: Record<string, TokenAccess>
  expires_days?: number
}

async function request(path: string, init?: RequestInit) {
  const response = await fetch(`/-/api/tokens${path}`, {
    ...init,
    headers: { "content-type": "application/json", ...init?.headers },
  })
  if (!response.ok) {
    const error = await response.json().catch(() => ({}))
    throw new Error(
      error.message ?? `Token request failed (${response.status})`
    )
  }
  if (response.status === 204) return
  return response.json()
}

export function listTokens(): Promise<{ tokens: TokenInfo[] }> {
  return request("")
}

export function createToken(
  token: NewToken
): Promise<{ token: TokenInfo; secret: string }> {
  return request("", { method: "POST", body: JSON.stringify(token) })
}

export function removeToken(id: number) {
  return request(`/${id}`, { method: "DELETE" })
}
