// Pushes and fetches: what one server accepts, every server serves.

import { expect, test } from "bun:test"
import { cluster } from "../harness/cluster"
import { Remote, Work } from "../harness/git"
import { newUser, repoName } from "../harness/world"

for (const via of ["http", "ssh"] as const) {
  test(`a push over ${via} is what a clone gets`, async () => {
    const owner = await newUser()
    const remote = new Remote(repoName(owner), owner, via)
    const work = await Work.init()
    await work.commit("First", { "README.md": "# Test\n" })
    const head = await work.commit("Second", { "src/main.txt": "hello\n" })
    await work.push(remote, "main")

    const clone = await Work.clone(remote)
    expect(await clone.head()).toBe(head)
    expect(await clone.git("status", "--porcelain")).toBe("")
    expect(await clone.git("rev-list", "--count", "HEAD")).toBe("2")
  })
}

test("the first push creates the repository with its default branch", async () => {
  const owner = await newUser()
  const remote = new Remote(repoName(owner), owner)
  const work = await Work.init()
  await work.git("checkout", "-q", "-b", "trunk")
  const head = await work.commit("First")
  await work.push(remote, "trunk")

  const refs = await remote.refs()
  expect(refs["refs/heads/trunk"]).toBe(head)
  expect(refs.HEAD).toBe(head)
})

test("pushes through the servers in turn all arrive", async () => {
  const owner = await newUser()
  const remote = new Remote(repoName(owner), owner)
  const work = await Work.init()
  let head = ""
  for (let i = 0; i < 6; i++) {
    head = await work.commit(`Commit ${i}`, { "count.txt": `${i}\n` })
    await work.push(remote, "main")
  }
  for (const server of cluster().servers) {
    expect((await remote.on(server).refs())["refs/heads/main"]).toBe(head)
  }
})

test("branches, tags, and deletions reach every server", async () => {
  const owner = await newUser()
  const remote = new Remote(repoName(owner), owner)
  const work = await Work.init()
  const first = await work.commit("First")
  await work.git("tag", "v1")
  await work.git("checkout", "-q", "-b", "feature")
  const feature = await work.commit("On the branch")
  await work.push(remote, "main", "feature", "v1")
  await work.push(remote, "--delete", "feature")
  await work.git("checkout", "-q", "-b", "other")
  await work.push(remote, "other")

  for (const server of cluster().servers) {
    const refs = await remote.on(server).refs()
    expect(refs["refs/heads/main"]).toBe(first)
    expect(refs["refs/tags/v1"]).toBe(first)
    expect(refs["refs/heads/other"]).toBe(feature)
    expect(refs["refs/heads/feature"]).toBeUndefined()
  }
})
