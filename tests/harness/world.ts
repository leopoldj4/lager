// Users and repository names of the tests. Each is new, so tests do not
// meet each other's data and test files can run in any order.

import { randomBytes } from "node:crypto"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { cluster } from "./cluster"
import { Remote, Work } from "./git"
import { run } from "./process"

export type User = {
  name: string
  password: string
  /** The private key for Git over SSH. */
  key: string
  /** The server's ID of that key. */
  keyId: string
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
  const added = await cluster().admin(["ssh-key", "add", name, `${key}.pub`])
  const keyId = /added SSH key (\S+) /.exec(added)?.[1]
  if (!keyId) throw new Error(`ssh-key add answered ${added}`)
  return { name, password, key, keyId }
}

/** A name for a new repository of `owner`. */
export const repoName = (owner: User) => `${owner.name}/r${unique()}`

/** A new repository of a new user with one commit on main. */
export async function newRepo() {
  const owner = await newUser()
  const repo = repoName(owner)
  const work = await Work.init()
  const head = await work.commit("First", { "a.txt": "a\n" })
  await work.push(new Remote(repo, owner), "main")
  return { owner, repo, work, head }
}

/**
 * A new access token of the user, with its password set to the token:
 * tokens work in place of passwords. `access` limits it to repositories,
 * as `repo=level`.
 */
export async function newToken(user: User, ...access: string[]): Promise<{ id: string; as: User }> {
  const flags = access.flatMap((a) => ["--access", a])
  const r = await cluster().attemptAdmin(["token", "create", user.name, `t${unique()}`, ...flags])
  const id = /created token (\S+) for/.exec(r.stderr)?.[1]
  const token = r.stdout.trim()
  if (r.code !== 0 || !id || !token) throw new Error(`token create failed (${r.code}):\n${r.stderr}`)
  return { id, as: { ...user, password: token } }
}
