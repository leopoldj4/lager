// Outages: a server that stops, loses its cache, or loses a service. The
// other servers go on, nothing accepted is lost, and the server serves
// again once the outage ends.

import { afterEach, expect, test } from "bun:test"
import { randomBytes } from "node:crypto"
import { writeFileSync } from "node:fs"
import { join } from "node:path"
import { cluster } from "../harness/cluster"
import { Remote, Work } from "../harness/git"
import { eventually } from "../harness/process"
import { newRepo } from "../harness/world"

afterEach(() => cluster().heal())

/** Whether the server answers a read of the remote. */
const serves = (remote: Remote) =>
  remote.refs().then(
    () => true,
    () => false
  )

test("a server with an empty cache serves everything", async () => {
  const { owner, repo, work } = await newRepo()
  const remote = new Remote(repo, owner)
  await work.git("tag", "v1")
  let head = await work.commit("Second", { "b.txt": "b\n" })
  await work.push(remote, "main", "v1")

  for (const server of cluster().servers) {
    await server.kill()
    server.emptyCache()
    await server.start()
    const clone = await Work.clone(remote.on(server))
    expect(await clone.head()).toBe(head)
    expect(await clone.git("tag", "--list")).toBe("v1")
    // And accepts the next push.
    head = await clone.commit(`Through server ${server.index}`)
    await clone.push(remote.on(server), "main")
  }
})

test("a server that stops during a push leaves the repository whole", async () => {
  const { owner, repo, work } = await newRepo()
  const remote = new Remote(repo, owner)
  const [victim, ...others] = cluster().servers
  /** A commit that takes a while to push: random bytes do not compress. */
  const large = async (message: string) => {
    writeFileSync(join(work.dir, "large.bin"), randomBytes(16 * 1024 * 1024))
    await work.git("add", "large.bin")
    return work.commit(message)
  }
  let before = await large("Whole")
  const start = performance.now()
  await work.push(remote.on(victim), "main")
  const whole = performance.now() - start
  // The push is cut at different points of its way, most of them near the
  // point where the server makes it final: a little later than a whole push
  // took, because the server has just started.
  for (const part of [0.2, 0.6, 0.96, 1.04, 1.12, 1.2, 1.28]) {
    const pushed = await large(`Cut after ${part} of the time`)
    const push = work.tryPush(remote.on(victim), "main")
    await Bun.sleep(whole * part)
    await victim.kill()
    const result = await push

    // The other servers know the outcome without the one that stopped.
    const heads = new Set<string>()
    for (const server of others) heads.add((await remote.on(server).refs())["refs/heads/main"])
    await victim.start()
    for (const server of cluster().servers) heads.add((await remote.on(server).refs())["refs/heads/main"])
    expect(heads.size).toBe(1)
    const [now] = heads
    // A push that the client saw accepted is there; one that failed may be.
    expect(result.code === 0 ? [pushed] : [before, pushed]).toContain(now)

    await work.push(remote, "main")
    const clone = await Work.clone(remote)
    expect(await clone.head()).toBe(pushed)
    expect((await clone.attempt(["fsck", "--strict"])).code).toBe(0)
    before = pushed
  }
})

test("a server without NATS serves what the others accept", async () => {
  const { owner, repo, work } = await newRepo()
  const remote = new Remote(repo, owner)
  const [apart, ...others] = cluster().servers
  apart.links.nats.cut()

  for (const server of [...others, apart]) {
    const head = await work.commit(`Through server ${server.index}`)
    await work.push(remote.on(server), "main")
    for (const reader of cluster().servers) {
      expect((await remote.on(reader).refs())["refs/heads/main"]).toBe(head)
    }
  }
  // The load balancer takes it out after five seconds, and back in later.
  await eventually(async () => expect(await apart.healthy()).toBe(false), 30_000)
  apart.links.nats.restore()
  await eventually(async () => expect(await apart.healthy()).toBe(true), 30_000)
})

test("a server without storage refuses, and serves again afterwards", async () => {
  const { owner, repo, work } = await newRepo()
  const remote = new Remote(repo, owner)
  const [apart, ...others] = cluster().servers
  apart.links.storage.cut()

  await work.commit("Second")
  expect(await serves(remote.on(apart))).toBe(false)
  expect((await work.tryPush(remote.on(apart), "main")).code).not.toBe(0)
  await eventually(async () => expect(await apart.healthy()).toBe(false), 30_000)
  for (const server of others) expect(await serves(remote.on(server))).toBe(true)

  apart.links.storage.restore()
  const head = await work.commit("Third")
  await eventually(() => work.push(remote.on(apart), "main"), 30_000)
  for (const server of cluster().servers) {
    expect((await remote.on(server).refs())["refs/heads/main"]).toBe(head)
  }
})

test("a server without the database refuses, and serves again afterwards", async () => {
  const { owner, repo, work } = await newRepo()
  const remote = new Remote(repo, owner)
  const [apart, ...others] = cluster().servers
  apart.links.postgres.cut()

  await work.commit("Second")
  expect(await serves(remote.on(apart))).toBe(false)
  expect((await work.tryPush(remote.on(apart), "main")).code).not.toBe(0)
  for (const server of others) expect(await serves(remote.on(server))).toBe(true)

  apart.links.postgres.restore()
  const head = await work.commit("Third")
  await eventually(() => work.push(remote.on(apart), "main"), 30_000)
  for (const server of cluster().servers) {
    expect((await remote.on(server).refs())["refs/heads/main"]).toBe(head)
  }
})
