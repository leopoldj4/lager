// A long history: after 200 entries a server writes a snapshot, and a
// server without a cache starts from it.

import { expect, test } from "bun:test"
import { cluster } from "../harness/cluster"
import { Remote, Work } from "../harness/git"
import { eventually } from "../harness/process"
import { newRepo } from "../harness/world"

const snapshots = async () => {
  let sum = 0
  for (const server of cluster().servers) sum += await server.counter("snapshots")
  return sum
}

test(
  "a server with an empty cache serves a history with a snapshot",
  async () => {
    const { owner, repo, work } = await newRepo()
    const remote = new Remote(repo, owner)
    const before = await snapshots()
    let head = ""
    for (let i = 0; i < 205; i++) {
      head = await work.commit(`Commit ${i}`, { "count.txt": `${i}\n` })
      await work.push(remote, "main")
    }
    // Servers look for due snapshots once a minute.
    await eventually(async () => expect(await snapshots()).toBeGreaterThan(before), 90_000)

    const [server] = cluster().servers
    await server.kill()
    server.emptyCache()
    await server.start()
    const clone = await Work.clone(remote.on(server))
    expect(await clone.head()).toBe(head)
    expect(await clone.git("rev-list", "--count", "HEAD")).toBe("206")
    expect((await clone.attempt(["fsck", "--strict"])).code).toBe(0)

    // More pushes on top of the snapshot, through every server.
    for (const through of cluster().servers) {
      head = await work.commit(`Through server ${through.index}`)
      await work.push(remote.on(through), "main")
    }
    for (const reader of cluster().servers) {
      expect((await remote.on(reader).refs())["refs/heads/main"]).toBe(head)
    }
  },
  300_000
)
