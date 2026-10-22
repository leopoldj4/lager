#!/usr/bin/env bun
// Local mode: SeaweedFS, PostgreSQL, and NATS in Docker
// (local.compose.yaml), one server built from this checkout with a
// development user and repository, and the Vite server for the web
// interface, which reloads on changes and forwards the API to the server.
//
//   scripts/local.ts         build, start, and serve until Ctrl-C
//   scripts/local.ts down    remove this checkout's containers and data
//
// Every checkout has its own Docker project, data, and ports, so worktrees
// run side by side. The configuration (lager.env), the SSH host key, and the
// cache live in .local/; the server keeps its ports between runs while they
// are free, and Vite takes 3000 or the next free port. Remove .local/cache at any time: the server recovers from
// storage.
//
// With Tailscale installed, `tailscale serve` also offers the web interface
// and the server over HTTPS, and Git over SSH, on this machine's tailnet name
// under the same port numbers, until the script stops.
//
// The script prints only the address and errors: the build's and Docker's
// output when they fail, the server's warnings, and Vite's errors.

import { $, type ShellPromise } from "bun"
import { createHash, randomBytes } from "node:crypto"
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { createServer } from "node:net"
import { homedir } from "node:os"
import { dirname, join } from "node:path"

const repo = dirname(import.meta.dir)
const root = join(repo, ".local")
const envFile = join(root, "lager.env")
const hostKey = join(root, "ssh_host_ed25519_key")
const project = `lager-local-${createHash("sha256").update(repo).digest("hex").slice(0, 8)}`
const compose = ["docker", "compose", "-p", project, "-f", join(import.meta.dir, "local.compose.yaml")]
const composeEnv = { ...process.env, LAGER_LOCAL_DIR: root }

if (process.argv[2] === "down") {
  await $`${compose} down -v`.env(composeEnv)
  process.exit(0)
}
if (process.argv[2]) {
  console.error("usage: scripts/local.ts [down]")
  process.exit(2)
}

mkdirSync(join(root, "cache"), { recursive: true })
const previous = readEnv()

await quiet($`bun install --frozen-lockfile`.cwd(join(repo, "web")))
await quiet($`cargo build --manifest-path ${join(repo, "Cargo.toml")} --bin lager --bin lager-server`)
const target = process.env.CARGO_TARGET_DIR ?? join(repo, "target")
// Vite serves the web interface; the server answers only its API, not an
// old build beside the programs.
rmSync(join(target, "share/lager/web"), { force: true })
const lager = join(target, "debug/lager")
const lagerServer = join(target, "debug/lager-server")

writeFileSync(
  join(root, "s3.json"),
  JSON.stringify({
    identities: [
      {
        name: "lager-dev",
        credentials: [{ accessKey: "lager-dev", secretKey: "lager-dev-secret" }],
        actions: ["Admin", "Read", "Write", "List", "Tagging"],
      },
    ],
  })
)
// Clients remember the host key, so it stays between runs.
if (!existsSync(hostKey)) await $`ssh-keygen -q -t ed25519 -N '' -C lager-local -f ${hostKey}`

// Docker picks the services' ports; they may change between runs.
await quiet($`${compose} up -d --wait`.env(composeEnv))
const port = async (service: string, inner: number) =>
  (await $`${compose} port ${service} ${inner}`.env(composeEnv).text()).trim()

const listen = await freePort(previous.LAGER_LISTEN, 8080)
const sshListen = await freePort(previous.LAGER_SSH_LISTEN, 2222)
const env: Record<string, string> = {
  LAGER_LISTEN: listen,
  LAGER_CACHE_DIR: join(root, "cache"),
  LAGER_S3_ENDPOINT: `http://${await port("storage", 8333)}`,
  LAGER_S3_BUCKET: "lager",
  LAGER_S3_ACCESS_KEY_ID: "lager-dev",
  LAGER_S3_SECRET_ACCESS_KEY: "lager-dev-secret",
  LAGER_S3_PREFIX: "local",
  LAGER_DATABASE_URL: `postgres://lager@${await port("postgres", 5432)}/lager`,
  LAGER_NATS_URL: `nats://${await port("nats", 4222)}`,
  // Sign-ins stay valid between runs.
  LAGER_SESSION_SECRET: previous.LAGER_SESSION_SECRET ?? randomBytes(32).toString("hex"),
  LAGER_SSH_LISTEN: sshListen,
}
// One line per variable, for `set -a; . .local/lager.env; set +a`.
writeFileSync(envFile, Object.entries(env).map(([k, v]) => `${k}=${v}\n`).join(""))
// GitHub mirrors: LAGER_GITHUB_CLIENT_ID and LAGER_GITHUB_CLIENT_SECRET pass
// through from the environment.
const serverEnv = {
  RUST_LOG: "warn",
  ...process.env,
  ...env,
  LAGER_SSH_HOST_KEY: readFileSync(hostKey, "utf8"),
}
const admin = (args: string[]) => $`${lager} ${args}`.env(serverEnv).quiet()

// The storage service answers a little after its container starts.
for (let i = 0; ; i++) {
  if ((await admin(["repo", "list"]).nothrow()).exitCode === 0) break
  if (i === 60) fail("the storage service does not answer")
  await Bun.sleep(1000)
}
if (!(await admin(["repo", "list"]).text()).split("\n").some((l) => l.startsWith("dev/demo"))) {
  await admin(["repo", "create", "dev/demo"])
}
if (!(await admin(["user", "list"]).text()).split("\n").some((l) => l.split("\t")[0] === "dev")) {
  await $`echo dev-password | ${lager} user create dev --access '*=force'`.env(serverEnv).quiet()
}
// Your own public key, so that Git over SSH works as dev.
const pub = ["id_ed25519.pub", "id_ecdsa.pub", "id_rsa.pub"].map((f) => join(homedir(), ".ssh", f)).find(existsSync)
if (pub) await admin(["ssh-key", "add", "dev", pub]).nothrow()

const vitePort = (await freePort(undefined, 3000)).split(":")[1]
const processes = [
  Bun.spawn([lagerServer], { env: serverEnv, stdio: ["ignore", "inherit", "inherit"] }),
  Bun.spawn([join(repo, "web/node_modules/.bin/vite"), "dev", "--host", "127.0.0.1", "--port", vitePort, "--strictPort", "--logLevel", "error"], {
    cwd: join(repo, "web"),
    env: { ...process.env, LAGER_URL: `http://${listen}` },
    stdio: ["ignore", "inherit", "inherit"],
  }),
]
// `tailscale serve` arguments that publish a local port on the tailnet, and
// the same arguments without the target remove it again.
const served: string[][] = []
let stopping = false
const stop = async (code: number) => {
  if (stopping) return
  stopping = true
  for (const s of served) await $`tailscale serve ${s} off`.quiet().nothrow()
  for (const p of processes) p.kill()
  await Promise.all(processes.map((p) => p.exited))
  await $`${compose} stop`.env(composeEnv).quiet().nothrow()
  process.exit(code)
}
process.on("SIGINT", () => stop(130))
process.on("SIGTERM", () => stop(143))
for (const p of processes) p.exited.then((code) => stop(code || 1))

for (const url of [`http://${listen}/_lager/health`, `http://127.0.0.1:${vitePort}/`]) {
  for (let i = 0; !(await fetch(url).then((r) => r.ok, () => false)); i++) {
    if (i === 240) {
      console.error(`${url} does not answer`)
      await stop(1)
    }
    await Bun.sleep(250)
  }
}
const tailnet = await tailnetName()
const httpPort = listen.split(":")[1]
const sshPort = sshListen.split(":")[1]
if (tailnet) {
  for (const [arg, target] of [
    [`--https=${vitePort}`, `http://127.0.0.1:${vitePort}`],
    [`--https=${httpPort}`, `http://${listen}`],
    [`--tcp=${sshPort}`, `tcp://${sshListen}`],
  ]) {
    served.push([arg])
    const r = await $`tailscale serve --bg ${arg} ${target}`.quiet().nothrow()
    if (r.exitCode !== 0) {
      console.error(`tailscale serve ${arg} ${target} failed:\n${r.stderr}`)
      await stop(1)
    }
  }
}
// Without Tailscale, the local address.
console.log(`lager is ready: ${tailnet ? `https://${tailnet}` : "http://127.0.0.1"}:${vitePort}/`)

/** This machine's name on the tailnet; none without a running Tailscale. */
async function tailnetName(): Promise<string | undefined> {
  if (!Bun.which("tailscale")) return undefined
  const r = await $`tailscale status --json`.quiet().nothrow()
  if (r.exitCode !== 0) return undefined
  const status = r.json()
  if (status.BackendState !== "Running") return undefined
  return status.Self?.DNSName?.replace(/\.$/, "") || undefined
}

/** The previous configuration, if any. */
function readEnv(): Record<string, string> {
  if (!existsSync(envFile)) return {}
  return Object.fromEntries(
    readFileSync(envFile, "utf8")
      .split("\n")
      .filter((l) => l.includes("="))
      .map((l) => [l.slice(0, l.indexOf("=")), l.slice(l.indexOf("=") + 1)])
  )
}

/** The previous address if its port is still free, else the first free port from `from`. */
async function freePort(previousAddr: string | undefined, from: number): Promise<string> {
  const previousPort = Number(previousAddr?.split(":").pop())
  if (previousPort && (await isFree(previousPort))) return `127.0.0.1:${previousPort}`
  for (let p = from; p < from + 100; p++) {
    if (await isFree(p)) return `127.0.0.1:${p}`
  }
  fail(`no free port from ${from}`)
}

function isFree(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const s = createServer()
    s.once("error", () => resolve(false))
    s.listen(port, "127.0.0.1", () => s.close(() => resolve(true)))
  })
}

/** Runs a command and shows its output only if it fails. */
async function quiet(command: ShellPromise) {
  const r = await command.quiet().nothrow()
  if (r.exitCode !== 0) fail(`${r.stdout}${r.stderr}`.trim())
}

function fail(message: string): never {
  console.error(message)
  process.exit(1)
}
