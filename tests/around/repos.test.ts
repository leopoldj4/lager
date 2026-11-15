// Repositories from the web interface: created, renamed, published, and
// deleted through one server, for all.

import { expect, test } from "bun:test"
import { cluster } from "../harness/cluster"
import { Remote, Work } from "../harness/git"
import { eventually } from "../harness/process"
import { Session } from "../harness/session"
import { newRepo, newUser, repoName } from "../harness/world"

/** The names of the user's repositories, as each server lists them. */
const lists = async (session: Session) => {
  const pages = await session.everywhere()
  const answers = await Promise.all(pages.map((p) => p.request({ op: "repos" })))
  for (const p of pages) p.close()
  return answers.map((a) => a.repos.map((r) => r.name).sort())
}

const everyServer = <T>(answer: T) => cluster().servers.map(() => answer)

test("a repository made on the web is there for every server and for Git", async () => {
  const owner = await newUser()
  const session = await Session.signIn(owner)
  const name = repoName(owner)
  const page = await session.live()
  const made = await page.request({ op: "repo_create", name })
  expect(made.repo).toMatchObject({ name, default_branch: "main", anonymous_read: false })
  await eventually(async () => expect(await lists(session)).toEqual(everyServer([name])))

  const work = await Work.init()
  const head = await work.commit("First")
  await work.push(new Remote(name, owner), "main")
  for (const other of await session.everywhere()) {
    const refs = await other.request({ op: "refs", repo: name })
    expect(refs.branches.map((b) => [b.name, b.oid])).toEqual([["main", head]])
    other.close()
  }
  expect(await page.refused({ op: "repo_create", name })).toBe("exists")
  page.close()
})

test("only the owner makes, renames, publishes, and deletes", async () => {
  const { owner, repo } = await newRepo()
  const writer = await newUser(`${repo}=write`)
  const page = await (await Session.signIn(writer)).live()
  expect(await page.refused({ op: "repo_create", name: `${owner.name}/other` })).toBe("forbidden")
  expect(await page.refused({ op: "repo_rename", repo, name: `${owner.name}/renamed` })).toBe("forbidden")
  expect(await page.refused({ op: "repo_visibility", repo, public: true })).toBe("forbidden")
  expect(await page.refused({ op: "repo_delete", repo })).toBe("forbidden")
  page.close()

  const visitor = await Session.visitor().live()
  expect(await visitor.refused({ op: "repo_delete", repo })).toBe("unauthorized")
  expect(await visitor.refused({ op: "repo_create", name: `${owner.name}/other` })).toBe("unauthorized")
  visitor.close()
})

test("a published repository is read without signing in, on every server", async () => {
  const { owner, repo, head } = await newRepo()
  const page = await (await Session.signIn(owner)).live()
  const visitors = await Session.visitor().everywhere()
  for (const v of visitors) expect(await v.refused({ op: "refs", repo })).toBe("not_found")

  expect((await page.request({ op: "repo_visibility", repo, public: true })).repo.anonymous_read).toBe(true)
  for (const v of visitors) {
    await eventually(async () => {
      const refs = await v.request({ op: "refs", repo })
      expect(refs.public).toBe(true)
      expect(refs.branches[0].oid).toBe(head)
    })
  }
  await page.request({ op: "repo_visibility", repo, public: false })
  for (const v of visitors) {
    await eventually(async () => expect(await v.refused({ op: "refs", repo })).toBe("not_found"))
    v.close()
  }
  page.close()
})

test("a renamed repository keeps its history under the new name only", async () => {
  const { owner, repo, head } = await newRepo()
  const session = await Session.signIn(owner)
  const page = await session.live()
  const name = repoName(owner)
  expect((await page.request({ op: "repo_rename", repo, name })).repo.name).toBe(name)
  page.close()

  for (const server of cluster().servers) {
    await eventually(async () => {
      expect((await new Remote(name, owner).on(server).refs())["refs/heads/main"]).toBe(head)
      await expect(new Remote(repo, owner).on(server).refs()).rejects.toThrow()
    })
  }
  await eventually(async () => expect(await lists(session)).toEqual(everyServer([name])))
})

test("a deleted repository is gone from every server", async () => {
  const { owner, repo } = await newRepo()
  const session = await Session.signIn(owner)
  const page = await session.live()
  await page.request({ op: "repo_delete", repo })
  page.close()
  await eventually(async () => expect(await lists(session)).toEqual(everyServer([])))
  for (const other of await session.everywhere()) {
    await eventually(async () => expect(await other.refused({ op: "refs", repo })).toBe("not_found"))
    other.close()
  }
})
