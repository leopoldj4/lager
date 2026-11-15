// Who may read and change a repository, on every server.

import { expect, test } from "bun:test"
import { cluster } from "../harness/cluster"
import { Remote, Work } from "../harness/git"
import { eventually } from "../harness/process"
import { newRepo, newUser, repoName } from "../harness/world"

test("a repository is private until it is made public", async () => {
  const { repo, head } = await newRepo()
  const stranger = await newUser()
  for (const server of cluster().servers) {
    await expect(new Remote(repo).on(server).refs()).rejects.toThrow()
    await expect(new Remote(repo, stranger).on(server).refs()).rejects.toThrow()
  }

  await cluster().admin(["repo", "set-public", repo, "true"])
  // Changes to the catalog apply within five seconds on every server.
  for (const server of cluster().servers) {
    await eventually(async () => {
      expect((await new Remote(repo).on(server).refs())["refs/heads/main"]).toBe(head)
    })
  }
  // Reading is not writing.
  const clone = await Work.clone(new Remote(repo))
  await clone.commit("Not mine")
  expect((await clone.tryPush(new Remote(repo, stranger), "main")).code).not.toBe(0)
})

test("a push cannot create a repository under another user", async () => {
  const [owner, other] = [await newUser(), await newUser()]
  const repo = repoName(owner)
  const work = await Work.init()
  await work.commit("First")
  for (const _ of cluster().servers) {
    expect((await work.tryPush(new Remote(repo, other), "main")).code).not.toBe(0)
  }
  await expect(new Remote(repo, owner).refs()).rejects.toThrow()
})

test("write access pushes but does not rewrite history", async () => {
  const { owner, repo, head } = await newRepo()
  const writer = await newUser(`${repo}=write`)
  const work = await Work.clone(new Remote(repo, writer))
  const next = await work.commit("Second", { "b.txt": "b\n" })
  await work.push(new Remote(repo, writer), "main")

  await work.git("reset", "-q", "--hard", head)
  const rewritten = await work.commit("Another second", { "c.txt": "c\n" })
  for (const _ of cluster().servers) {
    expect((await work.tryPush(new Remote(repo, writer), "--force", "main")).code).not.toBe(0)
  }
  expect((await new Remote(repo, owner).refs())["refs/heads/main"]).toBe(next)

  // The owner has `force` on their own repositories.
  await work.push(new Remote(repo, owner), "--force", "main")
  for (const server of cluster().servers) {
    expect((await new Remote(repo, owner).on(server).refs())["refs/heads/main"]).toBe(rewritten)
  }
})

test("read access clones but does not push", async () => {
  const { repo, head } = await newRepo()
  const reader = await newUser(`${repo}=read`)
  const work = await Work.clone(new Remote(repo, reader, "ssh"))
  expect(await work.head()).toBe(head)
  await work.commit("Second")
  expect((await work.tryPush(new Remote(repo, reader), "main")).code).not.toBe(0)
  expect((await work.tryPush(new Remote(repo, reader, "ssh"), "main")).code).not.toBe(0)
})
