// Access tokens and SSH keys from the settings pages: made through one
// server, they work, and stop working, on all.

import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { cluster } from "../harness/cluster"
import { Remote } from "../harness/git"
import { RestError, Session } from "../harness/session"
import { newRepo, newUser } from "../harness/world"

/** Whether each server lets the remote's user list the refs. */
const reads = (remote: Remote) =>
  Promise.all(
    cluster().servers.map((s) =>
      remote.on(s).refs().then(
        () => true,
        () => false
      )
    )
  )

const everyServer = <T>(answer: T) => cluster().servers.map(() => answer)

type Token = { id: number; name: string }
type Key = { id: number; name: string; fingerprint: string }

test("the settings need a session", async () => {
  for (const path of ["/tokens", "/ssh-keys", "/emails"]) {
    for (const _ of cluster().servers) {
      const answer = Session.visitor().rest("GET", path)
      expect(await answer.catch((e: RestError) => e.status)).toBe(401)
    }
  }
})

test("a token from the settings works on every server until it is removed", async () => {
  const { owner, repo } = await newRepo()
  const session = await Session.signIn(owner)
  const made = await session.rest<{ token: Token; secret: string }>("POST", "/tokens", { name: "laptop" })
  const remote = new Remote(repo, { ...owner, password: made.secret })
  expect(await reads(remote)).toEqual(everyServer(true))

  for (const _ of cluster().servers) {
    const { tokens } = await session.rest<{ tokens: Token[] }>("GET", "/tokens")
    expect(tokens.map((t) => t.name)).toEqual(["laptop"])
  }

  await session.rest("DELETE", `/tokens/${made.token.id}`)
  expect(await reads(remote)).toEqual(everyServer(false))
  expect((await session.rest<{ tokens: Token[] }>("GET", "/tokens")).tokens).toEqual([])
})

test("a token is limited to the repositories it names", async () => {
  const { owner, repo, work } = await newRepo()
  const session = await Session.signIn(owner)
  const made = await session.rest<{ secret: string }>("POST", "/tokens", { name: "ci", access: { [repo]: "read" } })
  const remote = new Remote(repo, { ...owner, password: made.secret })
  expect(await reads(remote)).toEqual(everyServer(true))
  await work.commit("Second")
  for (const _ of cluster().servers) {
    expect((await work.tryPush(remote, "main")).code).not.toBe(0)
  }
})

test("an SSH key from the settings works on every server until it is removed", async () => {
  const { repo } = await newRepo()
  // The key of a user who cannot read the repository, added to one who can.
  const [reader, other] = [await newUser(`${repo}=read`), await newUser()]
  await cluster().admin(["ssh-key", "delete", other.name, other.keyId])
  const session = await Session.signIn(reader)
  const made = await session.rest<{ key: Key }>("POST", "/ssh-keys", {
    key: readFileSync(`${other.key}.pub`, "utf8"),
    name: "second",
  })
  const remote = new Remote(repo, other, "ssh")
  expect(await reads(remote)).toEqual(everyServer(true))

  const { keys } = await session.rest<{ keys: Key[] }>("GET", "/ssh-keys")
  expect(keys.map((k) => k.name)).toContain("second")

  await session.rest("DELETE", `/ssh-keys/${made.key.id}`)
  expect(await reads(remote)).toEqual(everyServer(false))
})

test("a user sees and removes only their own tokens", async () => {
  const [one, other] = [await newUser(), await newUser()]
  const mine = await (await Session.signIn(one)).rest<{ token: Token }>("POST", "/tokens", { name: "mine" })
  const theirs = await Session.signIn(other)
  expect((await theirs.rest<{ tokens: Token[] }>("GET", "/tokens")).tokens).toEqual([])
  const removal = theirs.rest("DELETE", `/tokens/${mine.token.id}`)
  expect(await removal.catch((e: RestError) => e.status)).toBe(404)
})
