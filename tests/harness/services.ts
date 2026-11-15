// The services that the servers under test share: SeaweedFS, PostgreSQL,
// and NATS in Docker, from scripts/local.compose.yaml. Every checkout has
// its own Docker project, apart from the one of scripts/local.ts.

import { createHash } from "node:crypto"
import { mkdirSync, writeFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { attempt, run } from "./process"

export const repo = dirname(dirname(import.meta.dir))
/** Files of the tests; .local is not under version control. */
export const root = join(repo, ".local/test")

const project = `lager-test-${createHash("sha256").update(repo).digest("hex").slice(0, 8)}`
const base = ["docker", "compose", "-p", project, "-f", join(repo, "scripts/local.compose.yaml")]
const env = { LAGER_LOCAL_DIR: root }

export const compose = (...args: string[]) => run([...base, ...args], { env })

export const s3 = { accessKey: "lager-test", secretKey: "lager-test-secret", bucket: "lager" }

/** Starts the services; answers their addresses on this machine. */
export async function up() {
  mkdirSync(root, { recursive: true })
  writeFileSync(
    join(root, "s3.json"),
    JSON.stringify({
      identities: [
        {
          name: s3.accessKey,
          credentials: [{ accessKey: s3.accessKey, secretKey: s3.secretKey }],
          actions: ["Admin", "Read", "Write", "List", "Tagging"],
        },
      ],
    })
  )
  await compose("up", "-d", "--wait")
  // Docker picks the ports.
  const port = async (service: string, inner: number) => (await compose("port", service, String(inner))).trim()
  return {
    storage: await port("storage", 8333),
    postgres: await port("postgres", 5432),
    nats: await port("nats", 4222),
  }
}

const psql = (sql: string) => compose("exec", "-T", "postgres", "psql", "-U", "lager", "-d", "lager", "-qc", sql)

export const createDatabase = (name: string) => psql(`CREATE DATABASE "${name}"`)
export const dropDatabase = (name: string) => psql(`DROP DATABASE IF EXISTS "${name}" WITH (FORCE)`)

/** Stops the services and keeps their data. */
export const stop = () => attempt([...base, "stop"], { env })

/** Removes the services and their data. */
export const down = () => compose("down", "-v")
