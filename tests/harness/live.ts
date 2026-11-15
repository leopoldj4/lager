// The live connection as a page uses it: one WebSocket to one server for
// queries, changes, and change events
// (docs/decisions/0013-live-connection-protocol.md). Requests and answers
// have the types of web/src/lib/wire.ts.

import type { Ops } from "../../web/src/lib/wire"
import { cluster, type Server } from "./cluster"
import type { Session } from "./session"

/** A request: the name of an operation and its fields. */
export type Request<K extends keyof Ops = keyof Ops> = {
  [P in K]: { op: P } & Ops[P]["args"]
}[K]

export class LiveError extends Error {
  constructor(
    public code: string,
    message: string
  ) {
    super(`${code}: ${message}`)
  }
}

/** A message that the server sends by itself. */
export type Event =
  | { type: "changed"; repo: string; generation: number }
  | { type: "pull"; repo: string; number: number; version: number }
  | { type: "resync"; repo?: string }

type Waiter = { resolve: (v: any) => void; reject: (e: Error) => void }

export class Live {
  private nextId = 1
  private pending = new Map<number, Waiter>()
  /** Events not yet asked for, oldest first. */
  private events: Event[] = []
  /** The close code, once the connection has closed. */
  closed: number | undefined
  private waiting: { match: (e: Event) => boolean; resolve: (e: Event) => void }[] = []

  private constructor(
    private ws: WebSocket,
    readonly server: Server,
    /** Who the server takes the connection for; null for a visitor. */
    readonly user: string | null
  ) {
    ws.onmessage = (e) => this.receive(JSON.parse(e.data as string))
    ws.onclose = (e) => {
      this.closed = e.code
      for (const p of this.pending.values()) p.reject(new Error("the live connection closed"))
      this.pending.clear()
    }
  }

  /**
   * Connects to a server, the next one by default, with the session's
   * cookie. The connection is for the session's user, or for a visitor when
   * the server no longer accepts the cookie: `user` tells.
   */
  static async open(session: Session, server: Server = cluster().next()): Promise<Live> {
    const headers: Record<string, string> = session.cookie ? { cookie: session.cookie } : {}
    const ws = new WebSocket(`${server.http.replace(/^http/, "ws")}/-/api/live`, { headers })
    const welcome = await new Promise<any>((resolve, reject) => {
      ws.onopen = () => ws.send(JSON.stringify({ type: "hello", v: 1 }))
      ws.onmessage = (e) => resolve(JSON.parse(e.data as string))
      ws.onerror = () => reject(new Error(`the live connection to server ${server.index} failed`))
    })
    if (welcome.type !== "welcome") throw new Error(`live connection: ${JSON.stringify(welcome)}`)
    return new Live(ws, server, welcome.user)
  }

  private receive(m: any) {
    if (m.type === "changed" || m.type === "pull" || m.type === "resync") {
      const i = this.waiting.findIndex((w) => w.match(m))
      if (i < 0) this.events.push(m)
      else this.waiting.splice(i, 1)[0].resolve(m)
      return
    }
    const p = m.id != null ? this.pending.get(m.id) : undefined
    if (!p) return
    this.pending.delete(m.id)
    if (m.type === "error") p.reject(new LiveError(m.code, m.message))
    else p.resolve(m)
  }

  private send<T>(msg: Record<string, unknown>): Promise<T> {
    const id = this.nextId++
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve, reject })
      this.ws.send(JSON.stringify({ ...msg, id }))
    })
  }

  async request<K extends keyof Ops>(query: Request<K>): Promise<Ops[K]["answer"]> {
    return (await this.send<{ data: Ops[K]["answer"] }>({ type: "req", ...query })).data
  }

  /** A request that must fail; answers the error's code. */
  async refused<K extends keyof Ops>(query: Request<K>): Promise<string> {
    try {
      await this.request(query)
    } catch (e) {
      if (e instanceof LiveError) return e.code
      throw e
    }
    throw new Error(`${query.op} was answered`)
  }

  /** Asks for the change events of a repository; answers its generation now. */
  async watch(repo: string): Promise<number> {
    return (await this.send<{ generation: number }>({ type: "sub", repo })).generation
  }

  /** The oldest event that matches, received already or within `ms`. */
  event<E extends Event>(match: (e: Event) => e is E, ms = 15_000): Promise<E> {
    const i = this.events.findIndex(match)
    if (i >= 0) return Promise.resolve(this.events.splice(i, 1)[0] as E)
    return new Promise<E>((resolve, reject) => {
      const waiter = { match, resolve: resolve as (e: Event) => void }
      this.waiting.push(waiter)
      setTimeout(() => {
        this.waiting = this.waiting.filter((w) => w !== waiter)
        reject(new Error(`no such event within ${ms} ms`))
      }, ms)
    })
  }

  close() {
    this.ws.close(1000)
  }
}

/** Matches the change events of a repository. */
export const changed =
  (repo: string) =>
  (e: Event): e is Extract<Event, { type: "changed" }> =>
    e.type === "changed" && e.repo === repo

/** Matches the events of a pull request. */
export const pullChanged =
  (repo: string, number: number) =>
  (e: Event): e is Extract<Event, { type: "pull" }> =>
    e.type === "pull" && e.repo === repo && e.number === number
