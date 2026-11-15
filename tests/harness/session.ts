// A browser's session: the cookie that signing in sets, and the requests
// that carry it. Like a browser behind a load balancer, every request and
// every live connection goes to the next server.

import { cluster, type Server } from "./cluster"
import { Live } from "./live"
import type { User } from "./world"

/** An error answer of the API under `/-/api`. */
export class RestError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string
  ) {
    super(`${status} ${code}: ${message}`)
  }
}

export class Session {
  private constructor(
    /** `lager_session=<value>`; none for a visitor. */
    readonly cookie: string | undefined,
    readonly user: string | null
  ) {}

  /** Someone who is not signed in. */
  static visitor(): Session {
    return new Session(undefined, null)
  }

  static signIn(user: User, server?: Server): Promise<Session> {
    return Session.start("/session", user.name, user.password, server)
  }

  /** Creates the account, as the sign-up page does. */
  static signUp(name: string, password: string, server?: Server): Promise<Session> {
    return Session.start("/signup", name, password, server)
  }

  private static async start(path: string, user: string, password: string, server = cluster().next()) {
    const r = await fetch(`${server.http}/-/api${path}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ user, password }),
    })
    if (!r.ok) throw await error(r)
    const cookie = r.headers
      .getSetCookie()
      .map((c) => c.split(";")[0])
      .find((c) => /^lager_session=./.test(c))
    if (!cookie) throw new Error(`${path} as ${user}: no session cookie`)
    return new Session(cookie, user)
  }

  /** A request to `/-/api<path>`; answers its JSON. */
  async rest<T = unknown>(method: string, path: string, body?: unknown, server = cluster().next()): Promise<T> {
    const r = await this.fetch(`/-/api${path}`, server, {
      method,
      headers: { "content-type": "application/json" },
      ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
    })
    if (!r.ok) throw await error(r)
    const text = await r.text()
    return (text ? JSON.parse(text) : undefined) as T
  }

  /** A request for any path with the session's cookie. */
  fetch(path: string, server = cluster().next(), init: RequestInit = {}): Promise<Response> {
    const headers = { ...(init.headers as Record<string, string>), ...(this.cookie ? { cookie: this.cookie } : {}) }
    return fetch(`${server.http}${path}`, { ...init, headers, redirect: "manual" })
  }

  /** A live connection, as a page opens one. */
  live(server?: Server): Promise<Live> {
    return Live.open(this, server)
  }

  /** One live connection per server, in the order of the servers. */
  everywhere(): Promise<Live[]> {
    return Promise.all(cluster().servers.map((s) => this.live(s)))
  }
}

async function error(r: Response): Promise<RestError> {
  const e = (await r.json().catch(() => ({}))) as { code?: string; message?: string }
  return new RestError(r.status, e.code ?? "error", e.message ?? r.statusText)
}
