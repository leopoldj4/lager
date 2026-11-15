import { rest } from "./rest"

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

export function listTokens(): Promise<{ tokens: TokenInfo[] }> {
  return rest("/tokens", "Token request")
}

export function createToken(
  token: NewToken
): Promise<{ token: TokenInfo; secret: string }> {
  return rest("/tokens", "Token request", "POST", token)
}

export function removeToken(id: number) {
  return rest(`/tokens/${id}`, "Token request", "DELETE")
}
