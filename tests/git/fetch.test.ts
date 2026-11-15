// What clients fetch: every server serves the same objects.

import { expect, test } from "bun:test"
import { randomBytes } from "node:crypto"
import { writeFileSync } from "node:fs"
import { join } from "node:path"
import { cluster } from "../harness/cluster"
import { Remote, Work } from "../harness/git"
import { newRepo } from "../harness/world"

test("a fetch gets what was pushed since, from any server", async () => {
  const { owner, repo, work } = await newRepo()
  const remote = new Remote(repo, owner)
  const clone = await Work.clone(remote)
  for (let i = 0; i < 2 * cluster().servers.length; i++) {
    const head = await work.commit(`Commit ${i}`, { "count.txt": `${i}\n` })
    await work.push(remote, "main")
    await clone.fetch(remote, "main")
    expect(await clone.head("FETCH_HEAD")).toBe(head)
  }
  expect((await clone.attempt(["fsck", "--strict"])).code).toBe(0)
})

test("a shallow clone gets only the newest commit", async () => {
  const { owner, repo, work } = await newRepo()
  const head = await work.commit("Second")
  await work.push(new Remote(repo, owner), "main")
  for (const via of ["http", "ssh"] as const) {
    for (const server of cluster().servers) {
      const clone = await Work.init()
      await clone.fetch(new Remote(repo, owner, via).on(server), "--depth", "1", "main")
      expect(await clone.head("FETCH_HEAD")).toBe(head)
      expect(await clone.git("rev-list", "--count", "FETCH_HEAD")).toBe("1")
    }
  }
})

test("a large file comes back unchanged from every server", async () => {
  const { owner, repo, work } = await newRepo()
  // Random bytes do not compress.
  writeFileSync(join(work.dir, "large.bin"), randomBytes(24 * 1024 * 1024))
  await work.git("add", "large.bin")
  const head = await work.commit("A large file")
  const blob = await work.head("HEAD:large.bin")
  await work.push(new Remote(repo, owner), "main")
  for (const server of cluster().servers) {
    const clone = await Work.clone(new Remote(repo, owner).on(server))
    expect(await clone.head()).toBe(head)
    expect(await clone.git("hash-object", "large.bin")).toBe(blob)
  }
})

test("one push of many branches arrives whole", async () => {
  const { owner, repo, work, head } = await newRepo()
  const names = Array.from({ length: 300 }, (_, i) => `refs/heads/many/${i}`)
  for (const name of names) await work.git("update-ref", name, head)
  await work.push(new Remote(repo, owner), "refs/heads/many/*:refs/heads/many/*")
  for (const server of cluster().servers) {
    const refs = await new Remote(repo, owner).on(server).refs()
    expect(names.filter((n) => refs[n] !== head)).toEqual([])
  }
})
