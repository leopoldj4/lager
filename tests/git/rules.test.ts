// Ref rules and the refs that only the server writes.

import { expect, test } from "bun:test"
import { cluster } from "../harness/cluster"
import { Remote } from "../harness/git"
import { eventually } from "../harness/process"
import { newRepo, newUser } from "../harness/world"

test("a ref rule raises the level that a branch needs, on every server", async () => {
  const { owner, repo, work } = await newRepo()
  const writer = await newUser(`${repo}=write`)
  const remote = new Remote(repo, writer)
  await cluster().admin(["repo", "set-ref-rule", repo, "refs/heads/main", "--update", "force"])

  // Changes to the catalog apply within five seconds on every server; until
  // then a server still accepts the push.
  for (const server of cluster().servers) {
    await eventually(async () => {
      await work.commit("More")
      expect((await work.tryPush(remote.on(server), "main")).code).not.toBe(0)
    })
  }
  await work.push(remote, "main:refs/heads/topic")
  await work.push(new Remote(repo, owner), "main")

  await cluster().admin(["repo", "delete-ref-rule", repo, "refs/heads/main"])
  const third = await work.commit("Third")
  await eventually(async () => {
    await work.push(remote, "main")
  })
  for (const server of cluster().servers) {
    expect((await remote.on(server).refs())["refs/heads/main"]).toBe(third)
  }
})

test("moving a tag needs force", async () => {
  const { owner, repo, work, head } = await newRepo()
  const writer = await newUser(`${repo}=write`)
  await work.git("tag", "v1")
  await work.push(new Remote(repo, writer), "v1")

  const second = await work.commit("Second")
  await work.git("tag", "-f", "v1")
  for (const _ of cluster().servers) {
    expect((await work.tryPush(new Remote(repo, writer), "--force", "v1")).code).not.toBe(0)
  }
  expect((await new Remote(repo, owner).refs())["refs/tags/v1"]).toBe(head)
  await work.push(new Remote(repo, owner), "--force", "v1")
  for (const server of cluster().servers) {
    expect((await new Remote(repo, owner).on(server).refs())["refs/tags/v1"]).toBe(second)
  }
})

test("clients cannot push the refs that the server keeps", async () => {
  const { owner, repo, work } = await newRepo()
  const remote = new Remote(repo, owner)
  for (const ref of ["refs/pull/1/head", "refs/server/x"]) {
    for (const _ of cluster().servers) {
      expect((await work.tryPush(remote, `main:${ref}`)).code).not.toBe(0)
    }
  }
  expect(Object.keys(await remote.refs()).sort()).toEqual(["HEAD", "refs/heads/main"])
})

test("a new default branch is HEAD on every server", async () => {
  const { owner, repo, work, head } = await newRepo()
  const remote = new Remote(repo, owner)
  await work.git("checkout", "-q", "-b", "next")
  const next = await work.commit("On next")
  await work.push(remote, "next")
  expect((await remote.refs()).HEAD).toBe(head)

  await cluster().admin(["repo", "set-default-branch", repo, "next"])
  for (const server of cluster().servers) {
    await eventually(async () => {
      expect((await remote.on(server).refs()).HEAD).toBe(next)
    })
  }
})
