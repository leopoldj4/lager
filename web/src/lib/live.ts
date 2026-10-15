// The live connection to the worktree server: one WebSocket per tab that
// carries read queries and change events
// (docs/decisions/0013-live-connection-protocol.md).
//
// The connection reconnects by itself. Queries that were in flight are sent
// again, and every watched repository is watched again; a reconnect can
// miss events, so watchers hear `onResync`.

const VERSION = 1
// The server sends a heartbeat every 25 seconds.
const SILENCE_MS = 60_000
const MAX_BACKOFF_MS = 30_000
const CLOSE_SIGNED_OUT = 4401

export type Status = "connecting" | "open" | "offline"

export interface LiveState {
  status: Status
  /** Set after the first `welcome`. */
  ready: boolean
  user: string | null
  server: string | null
}

export class LiveError extends Error {
  constructor(
    public code: string,
    message: string
  ) {
    super(message)
  }
}

export interface Watcher {
  /** A newer generation exists than the one the page shows. */
  onChange: (generation: number) => void
  /** Events may be missing; fetch again. */
  onResync: () => void
  /** A pull request changed (docs/decisions/0019-pull-requests.md). */
  onPull?: (number: number) => void
}

type Pending = {
  msg: Record<string, unknown>
  resolve: (v: unknown) => void
  reject: (e: Error) => void
}

export class Live {
  private ws: WebSocket | null = null
  private nextId = 1
  private pending = new Map<number, Pending>()
  private watchers = new Map<string, Set<Watcher>>()
  /** Highest generation seen per repository. */
  private generations = new Map<string, number>()
  /** Highest version seen per pull request, by "<repo>#<number>". */
  private pullVersions = new Map<string, number>()
  private attempt = 0
  private timer: ReturnType<typeof setTimeout> | null = null
  private silence: ReturnType<typeof setTimeout> | null = null
  private listeners = new Set<() => void>()
  state: LiveState = {
    status: "connecting",
    ready: false,
    user: null,
    server: null,
  }

  constructor(private url: string) {}

  start() {
    if (!this.ws && !this.timer) this.connect()
  }

  subscribe = (listener: () => void) => {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }

  getState = () => this.state

  private set(patch: Partial<LiveState>) {
    this.state = { ...this.state, ...patch }
    this.listeners.forEach((l) => l())
  }

  /** Connect again at once, for example after signing in or out. */
  reconnect() {
    if (this.timer) clearTimeout(this.timer)
    this.timer = null
    this.attempt = 0
    const old = this.ws
    this.ws = null
    old?.close(1000)
    this.connect()
  }

  private connect() {
    this.timer = null
    this.set({ status: "connecting" })
    const ws = new WebSocket(this.url)
    this.ws = ws
    ws.onopen = () => {
      if (this.ws !== ws) return
      this.attempt = 0
      this.touch()
      this.send({ type: "hello", v: VERSION })
    }
    ws.onmessage = (e) => {
      if (this.ws !== ws) return
      this.touch()
      this.receive(JSON.parse(e.data as string))
    }
    ws.onclose = (e) => {
      if (this.ws !== ws) return
      this.ws = null
      if (this.silence) clearTimeout(this.silence)
      if (e.code === CLOSE_SIGNED_OUT) this.set({ user: null })
      this.set({ status: "offline" })
      this.later()
    }
  }

  /** Full jitter: a random delay up to min(30 s, 0.5 s · 2ⁿ). */
  private later() {
    const cap = Math.min(MAX_BACKOFF_MS, 500 * 2 ** this.attempt++)
    this.timer = setTimeout(() => this.connect(), Math.random() * cap)
  }

  /** Two missed heartbeats: the connection is dead even if open. */
  private touch() {
    if (this.silence) clearTimeout(this.silence)
    this.silence = setTimeout(() => this.ws?.close(4000), SILENCE_MS)
  }

  private send(msg: Record<string, unknown>) {
    if (this.ws?.readyState === WebSocket.OPEN)
      this.ws.send(JSON.stringify(msg))
  }

  private receive(m: any) {
    switch (m.type) {
      case "welcome":
        this.set({
          status: "open",
          ready: true,
          user: m.user,
          server: m.server,
        })
        for (const repo of this.watchers.keys())
          this.send({ type: "sub", repo })
        for (const [id, p] of this.pending) this.send({ ...p.msg, id })
        // Events between the connections are unknown.
        for (const set of this.watchers.values())
          set.forEach((w) => w.onResync())
        break
      case "res": {
        const p = this.pending.get(m.id)
        if (!p) return
        this.pending.delete(m.id)
        p.resolve(m.data)
        break
      }
      case "error": {
        const p = m.id != null ? this.pending.get(m.id) : undefined
        if (p) {
          this.pending.delete(m.id)
          p.reject(new LiveError(m.code, m.message))
        }
        break
      }
      case "subbed":
      case "changed":
        this.seen(m.repo, m.generation, true)
        break
      case "pull": {
        const key = `${m.repo}#${m.number}`
        if ((this.pullVersions.get(key) ?? 0) >= m.version) break
        this.pullVersions.set(key, m.version)
        this.watchers.get(m.repo)?.forEach((w) => w.onPull?.(m.number))
        break
      }
      case "resync": {
        const sets = m.repo
          ? [this.watchers.get(m.repo)]
          : [...this.watchers.values()]
        sets.forEach((set) => set?.forEach((w) => w.onResync()))
        if (!m.repo)
          for (const repo of this.watchers.keys())
            this.send({ type: "sub", repo })
        break
      }
    }
  }

  /** Records a generation; tells watchers when it is new. */
  seen(repo: string, generation: number, notify = false) {
    const known = this.generations.get(repo)
    if (known !== undefined && generation <= known) return
    this.generations.set(repo, generation)
    if (notify && known !== undefined)
      this.watchers.get(repo)?.forEach((w) => w.onChange(generation))
  }

  generation(repo: string) {
    return this.generations.get(repo)
  }

  request<T>(query: { op: string; [k: string]: unknown }): Promise<T> {
    this.start()
    const id = this.nextId++
    const msg = { type: "req", ...query }
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, {
        msg,
        resolve: resolve as (v: unknown) => void,
        reject,
      })
      if (this.state.ready) this.send({ ...msg, id })
    }).then((data) => {
      const d = data as { generation?: number }
      if (typeof query.repo === "string" && typeof d.generation === "number")
        this.seen(query.repo, d.generation)
      return data
    })
  }

  watch(repo: string, w: Watcher) {
    this.start()
    let set = this.watchers.get(repo)
    if (!set) {
      set = new Set()
      this.watchers.set(repo, set)
      this.send({ type: "sub", repo })
    }
    set.add(w)
    return () => {
      set.delete(w)
      if (set.size === 0) {
        this.watchers.delete(repo)
        this.send({ type: "unsub", repo })
      }
    }
  }
}

function liveUrl() {
  const proto = location.protocol === "https:" ? "wss:" : "ws:"
  return `${proto}//${location.host}/-/api/live`
}

export const live =
  typeof window === "undefined"
    ? (null as unknown as Live)
    : new Live(liveUrl())
