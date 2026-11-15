// Users and repository names of the tests. Each is new, so tests do not
// meet each other's data and test files can run in any order.

import { randomBytes } from "node:crypto"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { cluster } from "./cluster"
import { run } from "./process"

export type User = {
  name: string
  password: string
  /** The private key for Git over SSH. */
  key: string
}

const unique = () => randomBytes(5).toString("hex")

const dirs: string[] = []
process.on("exit", () => {
  for (const d of dirs) rmSync(d, { recursive: true, force: true })
})

/** A new directory that is removed when the tests end. */
export function tempDir(): string {
  const dir = mkdtempSync(join(tmpdir(), "lager-test-"))
  dirs.push(dir)
  return dir
}

/**
 * A new user with a password and an SSH key. `access` adds levels on
 * repositories of others, as `repo=level`.
 */
export async function newUser(...access: string[]): Promise<User> {
  const name = `u${unique()}`
  const password = `pw-${unique()}`
  const flags = access.flatMap((a) => ["--access", a])
  await cluster().admin(["user", "create", name, ...flags], `${password}\n`)
  const key = join(tempDir(), "id_ed25519")
  await run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", name, "-f", key])
  await cluster().admin(["ssh-key", "add", name, `${key}.pub`])
  return { name, password, key }
}

/** A name for a new repository of `owner`. */
export const repoName = (owner: User) => `${owner.name}/r${unique()}`
