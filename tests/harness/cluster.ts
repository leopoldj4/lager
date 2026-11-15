// The servers under test: several `lager-server` processes of this
// checkout's build that share one storage prefix, one database, and NATS,
// as servers behind a load balancer do. Every run has its own prefix and
// database.
//
// `LAGER_TEST_SERVERS` sets how many (default 2). Tests ask for
// `cluster.next()` on every call, so one user's requests reach different
// servers in turn.

import { randomBytes } from "node:crypto"
import { existsSync, mkdirSync, openSync, readFileSync, rmSync } from "node:fs"
import { createServer } from "node:net"
import { join } from "node:path"
import { eventually, run } from "./process"
import * as services from "./services"

const target = process.env.CARGO_TARGET_DIR ?? join(services.repo, "target")
const lager = join(target, "debug/lager")
const lagerServer = join(target, "debug/lager-server")

export class Server {
  private process: Bun.Subprocess | undefined

  constructor(
    readonly index: number,
    /** `http://127.0.0.1:<port>` */
    readonly http: string,
    /** `127.0.0.1:<port>` for Git over SSH. */
    readonly ssh: string,
    private env: Record<string, string>,
    /** The server's warnings and errors. */
    readonly log: string
  ) {}

  get cache(): string {
    return this.env.LAGER_CACHE_DIR
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
      if (code !== undefined) return
      const r = await fetch(`${this.http}/_lager/health`)
      if (!r.ok) throw new Error(`server ${this.index} is not healthy: ${r.status}`)
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

    const shared = {
      LAGER_S3_ENDPOINT: `http://${addr.storage}`,
      LAGER_S3_BUCKET: services.s3.bucket,
      LAGER_S3_ACCESS_KEY_ID: services.s3.accessKey,
      LAGER_S3_SECRET_ACCESS_KEY: services.s3.secretKey,
      LAGER_S3_PREFIX: `test/${id}`,
      LAGER_DATABASE_URL: `postgres://lager@${addr.postgres}/${database}`,
      LAGER_NATS_URL: `nats://${addr.nats}`,
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
      const env = {
        ...shared,
        LAGER_LISTEN: `127.0.0.1:${http}`,
        LAGER_SSH_LISTEN: `127.0.0.1:${ssh}`,
        LAGER_CACHE_DIR: cache,
      }
      servers.push(new Server(i, `http://127.0.0.1:${http}`, `127.0.0.1:${ssh}`, env, join(dir, `server-${i}.log`)))
    }
    const cluster = new Cluster(servers, { ...shared, LAGER_CACHE_DIR: join(dir, "admin") }, database)
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

  /** Runs an administration command (`lager ...`). */
  admin(args: string[], input?: string): Promise<string> {
    return run([lager, ...args], { env: this.env, input })
  }

  async stop() {
    await Promise.all(this.servers.map((s) => s.kill()))
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
