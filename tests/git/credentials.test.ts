// Passwords, tokens, SSH keys, and access levels live in the database: a
// change applies to the next request on every server, also on servers
// that accepted the credential a moment ago.

import { expect, test } from "bun:test"
import { cluster } from "../harness/cluster"
import { Remote } from "../harness/git"
import { newRepo, newToken, newUser } from "../harness/world"

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

const all = (answer: boolean) => cluster().servers.map(() => answer)

test("a token works in place of the password, within its limits", async () => {
  const { owner, repo, work } = await newRepo()
  const full = await newToken(owner)
  const limited = await newToken(owner, `${repo}=read`)
  await work.commit("Second")
  expect((await work.tryPush(new Remote(repo, limited.as), "main")).code).not.toBe(0)
  await work.push(new Remote(repo, full.as), "main")
  expect(await reads(new Remote(repo, limited.as))).toEqual(all(true))
})

test("a deleted token stops working on every server at once", async () => {
  const { owner, repo } = await newRepo()
  const token = await newToken(owner)
  expect(await reads(new Remote(repo, token.as))).toEqual(all(true))
  await cluster().admin(["token", "delete", owner.name, token.id])
  expect(await reads(new Remote(repo, token.as))).toEqual(all(false))
})

test("a changed password replaces the old one on every server at once", async () => {
  const { owner, repo } = await newRepo()
  expect(await reads(new Remote(repo, owner))).toEqual(all(true))
  const changed = { ...owner, password: "another-password" }
  await cluster().admin(["user", "set-password", owner.name], `${changed.password}\n`)
  expect(await reads(new Remote(repo, owner))).toEqual(all(false))
  expect(await reads(new Remote(repo, changed))).toEqual(all(true))
})

test("a deleted SSH key stops working on every server at once", async () => {
  const { owner, repo } = await newRepo()
  expect(await reads(new Remote(repo, owner, "ssh"))).toEqual(all(true))
  await cluster().admin(["ssh-key", "delete", owner.name, owner.keyId])
  expect(await reads(new Remote(repo, owner, "ssh"))).toEqual(all(false))
})

test("access taken away applies on every server at once", async () => {
  const { repo, work } = await newRepo()
  const writer = await newUser(`${repo}=write`)
  await work.commit("Second")
  await work.push(new Remote(repo, writer), "main")
  expect(await reads(new Remote(repo, writer))).toEqual(all(true))

  await cluster().admin(["user", "set-access", writer.name, `${repo}=read`])
  await work.commit("Third")
  for (const server of cluster().servers) {
    expect((await work.tryPush(new Remote(repo, writer).on(server), "main")).code).not.toBe(0)
  }
  await cluster().admin(["user", "set-access", writer.name])
  expect(await reads(new Remote(repo, writer))).toEqual(all(false))
})

test("a deleted user is refused on every server at once", async () => {
  const { repo } = await newRepo()
  const reader = await newUser(`${repo}=read`)
  expect(await reads(new Remote(repo, reader))).toEqual(all(true))
  expect(await reads(new Remote(repo, reader, "ssh"))).toEqual(all(true))
  await cluster().admin(["user", "delete", reader.name])
  expect(await reads(new Remote(repo, reader))).toEqual(all(false))
  expect(await reads(new Remote(repo, reader, "ssh"))).toEqual(all(false))
})
