// The servers under test: several `lager-server` processes of this
// checkout's build that share one storage prefix, one database, and NATS,
// as servers behind a load balancer do. Every run has its own prefix and
// database.
//
// `LAGER_TEST_SERVERS` sets how many (default 2). Tests ask for
// `cluster.next()` on every call, so one user's requests reach different
// servers in turn.
//
// Each server reaches each service through its own `Link`, and can be
// killed and started again, for tests of outages.

import { randomBytes } from "node:crypto"
import { existsSync, mkdirSync, openSync, readFileSync, rmSync } from "node:fs"
import { createServer } from "node:net"
import { join } from "node:path"
import { Link } from "./link"
import { attempt, eventually, run, type Output } from "./process"
import * as services from "./services"

const target = process.env.CARGO_TARGET_DIR ?? join(services.repo, "target")
const lager = join(target, "debug/lager")
const lagerServer = join(target, "debug/lager-server")

/** The connections of one server to the services. */
export type Links = { storage: Link; postgres: Link; nats: Link }

export class Server {
  private process: Bun.Subprocess | undefined

  constructor(
    readonly index: number,
    /** `http://127.0.0.1:<port>` */
    readonly http: string,
    /** `127.0.0.1:<port>` for Git over SSH. */
    readonly ssh: string,
    readonly links: Links,
    private env: Record<string, string>,
    /** The server's warnings and errors. */
    readonly log: string
  ) {}

  get cache(): string {
    return this.env.LAGER_CACHE_DIR
  }

  get running(): boolean {
    return this.process !== undefined
  }

  /** Whether the load balancer check passes. */
  async healthy(): Promise<boolean> {
    return fetch(`${this.http}/_lager/health`).then(
      (r) => r.ok,
      () => false
    )
  }

  /** A counter of `GET /_lager/metrics`, such as `snapshots`; 0 before its first count. */
  async counter(name: string): Promise<number> {
    const text = await (await fetch(`${this.http}/_lager/metrics`)).text()
    return Number(new RegExp(`^lager_${name}_total (\\d+)$`, "m").exec(text)?.[1] ?? 0)
  }

  /** Removes the cache, as a new machine has none. The server must not run. */
  emptyCache() {
    if (this.running) throw new Error(`server ${this.index} runs`)
    rmSync(this.cache, { recursive: true, force: true })
    mkdirSync(this.cache)
  }

  /** Starts the process and waits until the load balancer check passes. */
  async start() {
    const log = openSync(this.log, "a")
    this.process = Bun.spawn([lagerServer], {
      env: { ...process.env, ...this.env },
      stdio: ["ignore", log, log],
    })
    const exited = this.process.exited
    let code: number | undefined
    exited.then((c) => (code = c))
    await eventually(async () => {
      if (code === undefined && !(await this.healthy())) throw new Error(`server ${this.index} is not healthy`)
    }, 60_000)
    if (code !== undefined) throw new Error(`server ${this.index} stopped (${code}):\n${readFileSync(this.log, "utf8")}`)
  }

  /** Stops at once, as a crash does: nothing in flight finishes. */
  async kill() {
    this.process?.kill("SIGKILL")
    await this.process?.exited
    this.process = undefined
  }
}

export class Cluster {
  private turn = 0

  private constructor(
    readonly servers: Server[],
    /** The configuration of the administration commands. */
    private env: Record<string, string>,
    private database: string
  ) {}

  static async start(count: number): Promise<Cluster> {
    for (const bin of [lager, lagerServer]) {
      if (!existsSync(bin)) throw new Error(`${bin} is missing: run cargo build first`)
    }
    const addr = await services.up()
    const id = `${Date.now().toString(36)}${randomBytes(3).toString("hex")}`
    const dir = join(services.root, "runs", id)
    mkdirSync(dir, { recursive: true })
    const database = `test_${id}`
    await services.createDatabase(database)
    const hostKey = join(dir, "ssh_host_ed25519_key")
    await run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "lager-test", "-f", hostKey])

    const reach = (to: { storage: string; postgres: string; nats: string }) => ({
      LAGER_S3_ENDPOINT: `http://${to.storage}`,
      LAGER_DATABASE_URL: `postgres://lager@${to.postgres}/${database}`,
      LAGER_NATS_URL: `nats://${to.nats}`,
    })
    const shared = {
      LAGER_S3_BUCKET: services.s3.bucket,
      LAGER_S3_ACCESS_KEY_ID: services.s3.accessKey,
      LAGER_S3_SECRET_ACCESS_KEY: services.s3.secretKey,
      LAGER_S3_PREFIX: `test/${id}`,
      LAGER_SESSION_SECRET: randomBytes(32).toString("hex"),
      LAGER_SSH_HOST_KEY: readFileSync(hostKey, "utf8"),
      LAGER_SIGN_UP: "true",
      RUST_LOG: "warn",
    }
    const servers: Server[] = []
    for (let i = 0; i < count; i++) {
      const [http, ssh] = [await freePort(), await freePort()]
      const cache = join(dir, `server-${i}`)
      mkdirSync(cache)
      const links = {
        storage: await Link.start(addr.storage),
        postgres: await Link.start(addr.postgres),
        nats: await Link.start(addr.nats),
      }
      const env = {
        ...shared,
        ...reach({ storage: links.storage.address, postgres: links.postgres.address, nats: links.nats.address }),
        LAGER_LISTEN: `127.0.0.1:${http}`,
        LAGER_SSH_LISTEN: `127.0.0.1:${ssh}`,
        LAGER_CACHE_DIR: cache,
      }
      servers.push(new Server(i, `http://127.0.0.1:${http}`, `127.0.0.1:${ssh}`, links, env, join(dir, `server-${i}.log`)))
    }
    // The administration commands reach the services directly.
    const cluster = new Cluster(servers, { ...shared, ...reach(addr), LAGER_CACHE_DIR: join(dir, "admin") }, database)
    // The storage service answers a little after its container starts.
    await eventually(() => cluster.admin(["repo", "list"]), 60_000)
    // The first server creates the tables.
    await servers[0].start()
    await Promise.all(servers.slice(1).map((s) => s.start()))
    return cluster
  }

  /** The servers in turn. */
  next(): Server {
    return this.servers[this.turn++ % this.servers.length]
  }

  /** Ends every outage: restores the links and starts the servers that do not run. */
  async heal() {
    for (const s of this.servers) for (const l of Object.values(s.links)) l.restore()
    await Promise.all(this.servers.filter((s) => !s.running).map((s) => s.start()))
    await eventually(async () => {
      for (const s of this.servers) if (!(await s.healthy())) throw new Error(`server ${s.index} is not healthy`)
    }, 60_000)
  }

  /** Runs an administration command (`lager ...`). */
  admin(args: string[], input?: string): Promise<string> {
    return run([lager, ...args], { env: this.env, input })
  }

  /** An administration command, whatever its exit code. */
  attemptAdmin(args: string[], input?: string): Promise<Output> {
    return attempt([lager, ...args], { env: this.env, input })
  }

  async stop() {
    await Promise.all(this.servers.map((s) => s.kill()))
    for (const s of this.servers) for (const l of Object.values(s.links)) l.close()
    await services.dropDatabase(this.database)
    await services.stop()
    // The logs stay for a look after a failure; the caches are large.
    for (const s of this.servers) rmSync(s.cache, { recursive: true, force: true })
  }
}

/** A port that nothing listens on now. */
function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const s = createServer()
    s.once("error", reject)
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address() as { port: number }
      s.close(() => resolve(port))
    })
  })
}

declare global {
  var lagerCluster: Cluster | undefined
}

/** The cluster that harness/setup.ts started. */
export function cluster(): Cluster {
  if (!globalThis.lagerCluster) throw new Error("no cluster: run the tests with `bun test` in tests/")
  return globalThis.lagerCluster
}
