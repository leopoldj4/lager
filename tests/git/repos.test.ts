// Repositories come and go for every server alike.

import { expect, test } from "bun:test"
import { cluster } from "../harness/cluster"
import { Remote, Work } from "../harness/git"
import { eventually } from "../harness/process"
import { newRepo, newUser, repoName } from "../harness/world"

test("a deleted repository is gone on every server, and its name starts anew", async () => {
  const { owner, repo, head } = await newRepo()
  const remote = new Remote(repo, owner)
  for (const server of cluster().servers) await remote.on(server).refs()

  await cluster().admin(["repo", "delete", repo])
  for (const server of cluster().servers) {
    await eventually(async () => {
      // A push would create it again; a read must not find it.
      await expect(remote.on(server).refs()).rejects.toThrow()
    })
  }

  const work = await Work.init()
  const fresh = await work.commit("A new start")
  // A server that still holds the old record refuses, for up to five seconds.
  await eventually(() => work.push(remote, "main"))
  for (const server of cluster().servers) {
    const clone = await Work.clone(remote.on(server))
    expect(await clone.head()).toBe(fresh)
    expect((await clone.attempt(["cat-file", "-e", head])).code).not.toBe(0)
  }
})

test("first pushes at once through different servers make one repository", async () => {
  const servers = cluster().servers
  for (let round = 0; round < 3; round++) {
    const owner = await newUser()
    const remote = new Remote(repoName(owner), owner)
    const branches = ["a", "b", "c"]
    const pushes = await Promise.all(
      branches.map(async (branch, i) => {
        const work = await Work.init()
        await work.git("checkout", "-q", "-b", branch)
        const head = await work.commit(`On ${branch}`, { [`${branch}.txt`]: "x\n" })
        const result = await work.tryPush(remote.on(servers[i % servers.length]), branch)
        return { branch, head, accepted: result.code === 0 }
      })
    )
    const accepted = pushes.filter((p) => p.accepted)
    expect(accepted.length).toBeGreaterThan(0)
    for (const server of servers) {
      const refs = await remote.on(server).refs()
      for (const p of accepted) expect(refs[`refs/heads/${p.branch}`]).toBe(p.head)
    }
  }
})
