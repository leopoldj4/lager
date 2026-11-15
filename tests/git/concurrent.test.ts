// Pushes at the same time through different servers: the head in storage
// orders them, and every server agrees on the result.

import { expect, test } from "bun:test"
import { cluster } from "../harness/cluster"
import { Remote, Work } from "../harness/git"
import { newUser, repoName } from "../harness/world"

test("of two pushes to one branch at once, one wins everywhere", async () => {
  const owner = await newUser()
  const remote = new Remote(repoName(owner), owner)
  const base = await Work.init()
  await base.commit("First")
  await base.push(remote, "main")

  const servers = cluster().servers
  for (let round = 0; round < 5; round++) {
    const works = await Promise.all([0, 1].map(() => Work.clone(remote)))
    const heads = await Promise.all(works.map((w, i) => w.commit(`Round ${round}, push ${i}`, { [`${i}.txt`]: `${round}\n` })))
    const results = await Promise.all(works.map((w, i) => w.tryPush(remote.on(servers[i % servers.length]), "main")))

    const accepted = results.flatMap((r, i) => (r.code === 0 ? [heads[i]] : []))
    expect(accepted).toHaveLength(1)
    for (const server of servers) {
      expect((await remote.on(server).refs())["refs/heads/main"]).toBe(accepted[0])
    }
  }
})

test("pushes to different branches at once are all accepted", async () => {
  const owner = await newUser()
  const remote = new Remote(repoName(owner), owner)
  const base = await Work.init()
  await base.commit("First")
  await base.push(remote, "main")

  const servers = cluster().servers
  const branches = ["a", "b", "c", "d"]
  const heads = await Promise.all(
    branches.map(async (branch, i) => {
      const work = await Work.clone(remote)
      await work.git("checkout", "-q", "-b", branch)
      const head = await work.commit(`On ${branch}`, { [`${branch}.txt`]: "x\n" })
      await work.push(remote.on(servers[i % servers.length]), branch)
      return head
    })
  )
  for (const server of servers) {
    const refs = await remote.on(server).refs()
    branches.forEach((branch, i) => expect(refs[`refs/heads/${branch}`]).toBe(heads[i]))
  }
})
