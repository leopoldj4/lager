// Signing in and up: a session of one server is a session of all.

import { expect, test } from "bun:test"
import { randomBytes } from "node:crypto"
import { cluster } from "../harness/cluster"
import { Remote, Work } from "../harness/git"
import { RestError, Session } from "../harness/session"
import { newUser } from "../harness/world"

/** Who each server takes the session for. */
const users = async (session: Session) => {
  const pages = await session.everywhere()
  for (const p of pages) p.close()
  return pages.map((p) => p.user)
}

const everyServer = <T>(answer: T) => cluster().servers.map(() => answer)

test("a session is valid on every server", async () => {
  const user = await newUser()
  for (const server of cluster().servers) {
    const session = await Session.signIn(user, server)
    expect(await users(session)).toEqual(everyServer(user.name))
  }
  expect(await users(Session.visitor())).toEqual(everyServer(null))
})

test("a wrong password signs nobody in", async () => {
  const user = await newUser()
  for (const server of cluster().servers) {
    const attempt = Session.signIn({ ...user, password: "wrong" }, server)
    expect(await attempt.catch((e: RestError) => e.code)).toBe("bad_credentials")
  }
})

test("a changed password ends the sessions on every server", async () => {
  const user = await newUser()
  const session = await Session.signIn(user)
  const pages = await session.everywhere()
  await cluster().admin(["user", "set-password", user.name], "another-password\n")
  expect(await users(session)).toEqual(everyServer(null))
  // The pages that were open hear of it at their next request.
  for (const page of pages) {
    await page.request({ op: "repos" }).catch(() => {})
    page.close()
  }
})

test("a deleted user's session is a visitor's on every server", async () => {
  const user = await newUser()
  const session = await Session.signIn(user)
  expect(await users(session)).toEqual(everyServer(user.name))
  await cluster().admin(["user", "delete", user.name])
  expect(await users(session)).toEqual(everyServer(null))
})

test("signing up makes an account that every server knows", async () => {
  const name = `s${randomBytes(5).toString("hex")}`
  const password = "sign-up-password"
  const session = await Session.signUp(name, password)
  expect(await users(session)).toEqual(everyServer(name))

  // The name is taken now, whichever server is asked.
  for (const server of cluster().servers) {
    const again = Session.signUp(name, "another-password", server)
    expect(await again.catch((e: RestError) => e.status)).toBe(409)
  }

  // The account owns the repositories under its name.
  const user = { name, password, key: "", keyId: "" }
  const work = await Work.init()
  const head = await work.commit("First")
  const remote = new Remote(`${name}/first`, user)
  await work.push(remote, "main")
  for (const server of cluster().servers) {
    expect((await remote.on(server).refs())["refs/heads/main"]).toBe(head)
  }
})
