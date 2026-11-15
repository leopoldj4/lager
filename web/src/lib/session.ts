// Signing in, up, and out. Each changes the session cookie, so the live
// connection connects again and the cached answers go.

import { live } from "./live"
import { queryClient } from "./query"
import { rest } from "./rest"

/** Connects again as the new user and drops the answers for the old one. */
export function sessionChanged() {
  live.reconnect()
  queryClient.clear()
}

/**
 * Which ways of signing up the server offers; passkeys need its domain.
 * `github`: GitHub signs in, and signs up with `enabled`.
 */
export async function signUpOptions(): Promise<{
  enabled: boolean
  passkey: boolean
  github: boolean
}> {
  const r = await fetch("/-/api/signup")
  if (!r.ok) return { enabled: false, passkey: false, github: false }
  return r.json()
}

/** The page that signs in with GitHub, then goes to `next`. */
export function githubSignInHref(next?: string) {
  return next
    ? `/-/api/github/sign-in?next=${encodeURIComponent(next)}`
    : "/-/api/github/sign-in"
}

/** The GitHub login of the sign-up that GitHub just named; null once expired. */
export async function githubSignUpLogin(): Promise<string | null> {
  const r = await fetch("/-/api/github/sign-up")
  if (!r.ok) return null
  return (await r.json()).login
}

/** Creates the account of a GitHub sign-up, and signs in. */
export async function signUpWithGithub(user: string) {
  await rest("/github/sign-up", "sign-up", "POST", { user })
  sessionChanged()
}

/** Creates an account with a password, and signs in. */
export async function signUpWithPassword(user: string, password: string) {
  await rest("/signup", "sign-up", "POST", { user, password })
  sessionChanged()
}

export async function signIn(user: string, password: string) {
  await rest("/session", "sign-in", "POST", { user, password })
  sessionChanged()
}

export async function signOut() {
  await fetch("/-/api/session", { method: "DELETE" })
  sessionChanged()
}
