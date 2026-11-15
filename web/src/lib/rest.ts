// JSON requests to the server's API under `/-/api`, for what does not go
// over the live connection: signing in and the account's settings.

import { LiveError } from "./live"

/** `what` names the request in the error when the server gives no message. */
export async function rest(
  path: string,
  what: string,
  method = "GET",
  body?: unknown
) {
  const r = await fetch(`/-/api${path}`, {
    method,
    headers: { "content-type": "application/json" },
    ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
  })
  if (!r.ok) {
    const e = await r.json().catch(() => ({}))
    throw new LiveError(
      e.code ?? "error",
      e.message ?? `${what} failed (${r.status})`
    )
  }
  const text = await r.text()
  return text ? JSON.parse(text) : undefined
}
