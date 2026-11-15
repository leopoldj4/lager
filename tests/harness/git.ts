// Git as a client runs it: the `git` program against the servers, over
// HTTP or SSH.

import { mkdirSync, writeFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { cluster, type Server } from "./cluster"
import { attempt, run, type Output } from "./process"
import { tempDir, type User } from "./world"

const env = {
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_CONFIG_NOSYSTEM: "1",
  GIT_TERMINAL_PROMPT: "0",
  GIT_AUTHOR_NAME: "Test",
  GIT_AUTHOR_EMAIL: "test@example.com",
  GIT_COMMITTER_NAME: "Test",
  GIT_COMMITTER_EMAIL: "test@example.com",
}

/**
 * A repository on the cluster as one user reaches it; without a user, as
 * anyone does. Every Git command goes to the next server, unless `on`
 * names one.
 */
export class Remote {
  constructor(
    readonly repo: string,
    readonly as?: User,
    readonly via: "http" | "ssh" = "http",
    private server?: Server
  ) {}

  on(server: Server): Remote {
    return new Remote(this.repo, this.as, this.via, server)
  }

  url(): string {
    const server = this.server ?? cluster().next()
    if (this.via === "ssh") return `ssh://git@${server.ssh}/${this.repo}.git`
    const url = new URL(`${server.http}/${this.repo}.git`)
    if (this.as) {
      url.username = this.as.name
      url.password = this.as.password
    }
    return url.href
  }

  env(): Record<string, string> {
    if (this.via !== "ssh" || !this.as) return {}
    // Only this key, and no record of the host keys of the tests.
    const ssh = `ssh -i ${this.as.key} -o IdentitiesOnly=yes -o IdentityAgent=none -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR`
    return { GIT_SSH_COMMAND: ssh }
  }

  /** The refs that the next server offers, by name. */
  async refs(): Promise<Record<string, string>> {
    const out = await run(["git", "-c", "credential.helper=", "ls-remote", this.url()], { env: { ...env, ...this.env() } })
    return Object.fromEntries(
      out
        .split("\n")
        .filter(Boolean)
        .map((l) => l.split("\t").reverse() as [string, string])
    )
  }
}

/** A local repository. */
export class Work {
  private constructor(readonly dir: string) {}

  static async init(): Promise<Work> {
    const w = new Work(tempDir())
    await w.git("init", "-q", "--initial-branch=main")
    return w
  }

  static async clone(remote: Remote): Promise<Work> {
    const w = new Work(tempDir())
    const r = await w.attempt(["-c", "credential.helper=", "clone", "-q", remote.url(), "."], remote.env())
    if (r.code !== 0) throw new Error(`clone of ${remote.repo} failed (${r.code}):\n${r.stderr}`)
    return w
  }

  /** Runs `git`, whatever its exit code. */
  attempt(args: string[], more: Record<string, string> = {}): Promise<Output> {
    return attempt(["git", ...args], { cwd: this.dir, env: { ...env, ...more } })
  }

  async git(...args: string[]): Promise<string> {
    return (await run(["git", ...args], { cwd: this.dir, env })).trim()
  }

  /** Writes the files and commits them; answers the commit. */
  async commit(message: string, files: Record<string, string> = {}): Promise<string> {
    for (const [path, content] of Object.entries(files)) {
      mkdirSync(dirname(join(this.dir, path)), { recursive: true })
      writeFileSync(join(this.dir, path), content)
      await this.git("add", "--", path)
    }
    await this.git("commit", "-q", "--allow-empty", "-m", message)
    return this.head()
  }

  head(rev = "HEAD"): Promise<string> {
    return this.git("rev-parse", rev)
  }

  /** `git push` with the given arguments, whatever the server answers. */
  tryPush(remote: Remote, ...args: string[]): Promise<Output> {
    return this.attempt(["-c", "credential.helper=", "push", "-q", remote.url(), ...args], remote.env())
  }

  /** A push that the server must accept. */
  async push(remote: Remote, ...args: string[]) {
    const r = await this.tryPush(remote, ...args)
    if (r.code !== 0) throw new Error(`push to ${remote.repo} failed (${r.code}):\n${r.stderr}`)
  }

  async fetch(remote: Remote, ...args: string[]) {
    const r = await this.attempt(["-c", "credential.helper=", "fetch", "-q", remote.url(), ...args], remote.env())
    if (r.code !== 0) throw new Error(`fetch from ${remote.repo} failed (${r.code}):\n${r.stderr}`)
  }
}
