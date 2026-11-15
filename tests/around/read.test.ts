// The pages' reads: every server answers the same for a repository.

import { expect, test } from "bun:test"
import { cluster } from "../harness/cluster"
import { Remote, Work } from "../harness/git"
import { Session } from "../harness/session"
import { newUser, repoName } from "../harness/world"

test("files, history, commits, and comparisons are the same on every server", async () => {
  const owner = await newUser()
  const repo = repoName(owner)
  const remote = new Remote(repo, owner)
  const work = await Work.init()
  const first = await work.commit("First", { "README.md": "# Read me\n", "src/a.txt": "one\n" })
  await work.git("tag", "v1")
  const second = await work.commit("Second\n\nWith a body.", { "src/a.txt": "one\ntwo\n", "src/b.bin": "\0\x01\x02" })
  await work.git("checkout", "-q", "-b", "topic")
  const topic = await work.commit("On the topic", { "src/c.txt": "c\n" })
  await work.push(remote, "main", "topic", "v1")

  const session = await Session.signIn(owner)
  for (const page of await session.everywhere()) {
    const refs = await page.request({ op: "refs", repo })
    expect(refs.head).toBe("main")
    expect(refs.branches.map((b) => [b.name, b.oid]).sort()).toEqual([
      ["main", second],
      ["topic", topic],
    ])
    expect(refs.tags.map((t) => [t.name, t.oid])).toEqual([["v1", first]])

    const root = await page.request({ op: "tree", repo, rev: "main" })
    expect(root.entries.map((e) => [e.name, e.kind])).toEqual([
      ["src", "tree"],
      ["README.md", "file"],
    ])
    expect(root.last_commit?.oid).toBe(second)
    const src = await page.request({ op: "tree", repo, rev: "v1", path: "src" })
    expect(src.entries.map((e) => e.name)).toEqual(["a.txt"])

    const text = await page.request({ op: "blob", repo, rev: "main", path: "src/a.txt" })
    expect(text).toMatchObject({ text: "one\ntwo\n", binary: false, size: 8 })
    expect(text.last_commit?.oid).toBe(second)
    const binary = await page.request({ op: "blob", repo, rev: "main", path: "src/b.bin" })
    expect(binary).toMatchObject({ text: null, binary: true })
    expect(await page.refused({ op: "blob", repo, rev: "main", path: "missing" })).toBe("not_found")

    const log = await page.request({ op: "log", repo, rev: "topic", limit: 2 })
    expect(log.commits.map((c) => c.oid)).toEqual([topic, second])
    expect(log.more).toBe(true)
    const history = await page.request({ op: "log", repo, rev: "main", path: "README.md" })
    expect(history.commits.map((c) => c.oid)).toEqual([first])

    const commit = await page.request({ op: "commit", repo, rev: second })
    expect(commit.commit).toMatchObject({ subject: "Second", message: "Second\n\nWith a body.", parents: [first] })
    expect(commit.diff).toContain("+two")

    const compared = await page.request({ op: "compare", repo, base: "main", head: "topic" })
    expect(compared.merge_base).toBe(second)
    expect(compared.commits.map((c) => c.oid)).toEqual([topic])
    expect(compared.diff).toContain("src/c.txt")
    page.close()
  }

  for (const server of cluster().servers) {
    const raw = await session.fetch(`/${repo}/-/raw/main/src/a.txt`, server)
    expect(raw.status).toBe(200)
    expect(await raw.text()).toBe("one\ntwo\n")
    expect((await Session.visitor().fetch(`/${repo}/-/raw/main/src/a.txt`, server)).status).toBe(404)
  }
})

test("a page reads what a push just changed, whichever server it asks", async () => {
  const owner = await newUser()
  const repo = repoName(owner)
  const remote = new Remote(repo, owner)
  const work = await Work.init()
  const pages = await (await Session.signIn(owner)).everywhere()
  for (let i = 0; i < 2 * pages.length; i++) {
    const head = await work.commit(`Commit ${i}`, { "count.txt": `${i}\n` })
    await work.push(remote, "main")
    // No waiting for an event: the answer must have the push already.
    const answers = await Promise.all(pages.map((p) => p.request({ op: "blob", repo, rev: "main", path: "count.txt" })))
    expect(answers.map((a) => [a.oid, a.text])).toEqual(pages.map(() => [head, `${i}\n`]))
  }
  for (const p of pages) p.close()
})
