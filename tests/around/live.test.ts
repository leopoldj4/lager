// The live connection: pages on one server see what another accepted.

import { expect, test } from "bun:test"
import { cluster } from "../harness/cluster"
import { Remote, Work } from "../harness/git"
import { Live, type Event } from "../harness/live"
import { newUser, repoName } from "../harness/world"

test("a push through one server reaches the pages of every server", async () => {
  const owner = await newUser()
  const repo = repoName(owner)
  const remote = new Remote(repo, owner)
  const work = await Work.init()
  await work.commit("First")
  await work.push(remote, "main")

  const servers = cluster().servers
  const pages = await Promise.all(servers.map((s) => Live.open(owner, s)))
  const before = await Promise.all(pages.map((p) => p.watch(repo)))

  const head = await work.commit("Second", { "a.txt": "a\n" })
  await work.push(remote.on(servers[0]), "main")

  for (const [i, page] of pages.entries()) {
    const changed = (e: Event): e is Extract<Event, { type: "changed" }> => e.type === "changed" && e.repo === repo
    expect((await page.event(changed)).generation).toBeGreaterThan(before[i])
    const refs = await page.request({ op: "refs", repo })
    expect(refs.branches.find((b) => b.name === "main")?.oid).toBe(head)
    const log = await page.request({ op: "log", repo, rev: "main" })
    expect(log.commits.map((c) => c.subject)).toEqual(["Second", "First"])
    page.close()
  }
})
