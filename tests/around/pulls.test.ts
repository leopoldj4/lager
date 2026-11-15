// Pull requests: opened, discussed, and merged through any server, with
// one result for all.

import { expect, test } from "bun:test"
import { cluster } from "../harness/cluster"
import { Remote, Work } from "../harness/git"
import { pullChanged, type Live } from "../harness/live"
import { eventually } from "../harness/process"
import { Session } from "../harness/session"
import { newRepo, newUser, type User } from "../harness/world"

/** A repository with a branch `topic` one commit ahead of main. */
async function withTopic(files: Record<string, string> = { "topic.txt": "topic\n" }) {
  const made = await newRepo()
  const remote = new Remote(made.repo, made.owner)
  await made.work.git("checkout", "-q", "-b", "topic")
  const topic = await made.work.commit("On the topic", files)
  await made.work.push(remote, "topic")
  await made.work.git("checkout", "-q", "main")
  return { ...made, remote, topic }
}

const page = async (user: User) => (await Session.signIn(user)).live()

/** Runs `use` with one page of the user per server, then closes them. */
async function onEveryServer<T>(user: User, use: (pages: Live[]) => Promise<T>): Promise<T> {
  const pages = await (await Session.signIn(user)).everywhere()
  try {
    return await use(pages)
  } finally {
    for (const p of pages) p.close()
  }
}

const uuid = () => crypto.randomUUID()

test("a pull request is opened, discussed, and merged through different servers", async () => {
  const { owner, repo, remote, head, topic } = await withTopic()
  const reader = await newUser(`${repo}=read`)

  // Readers open and comment.
  const theirs = await page(reader)
  const opened = await theirs.request({ op: "pull_create", repo, source: "topic", target: "main", title: "Topic", body: "Please." })
  expect(opened.pull).toMatchObject({ number: 1, state: "open", author: reader.name, source: "topic", target: "main", head_oid: topic })
  theirs.close()

  await onEveryServer(owner, async (pages) => {
    for (const p of pages) {
      const detail = await p.request({ op: "pull", repo, number: 1 })
      expect(detail).toMatchObject({ mergeable: "clean", conflicts: [], can_merge: true, source_oid: topic, target_oid: head })
      expect(detail.pull.title).toBe("Topic")
      const list = await p.request({ op: "pulls", repo })
      expect(list.pulls.map((x) => x.number)).toEqual([1])
      expect(list.counts).toEqual({ open: 1, closed: 0 })
    }
  })
  // The server keeps a ref at the source.
  for (const server of cluster().servers) {
    await eventually(async () => expect((await remote.on(server).refs())["refs/pull/1/head"]).toBe(topic))
  }

  // One comment through each server, by both.
  for (const user of [reader, owner]) {
    await onEveryServer(user, async (pages) => {
      for (const p of pages) await p.request({ op: "pull_comment", repo, number: 1, body: `From server ${p.server.index}` })
    })
  }
  const comments = cluster().servers.flatMap((s) => [reader, owner].map(() => `From server ${s.index}`)).sort()

  const mine = await page(owner)
  expect(await (await page(reader)).refused({ op: "pull_merge", repo, number: 1, head: topic, request: uuid() })).toBe("forbidden")
  const merged = await mine.request({ op: "pull_merge", repo, number: 1, head: topic, request: uuid() })
  expect(merged.pull).toMatchObject({ state: "merged", merged_by: owner.name })
  mine.close()

  await onEveryServer(owner, async (pages) => {
    for (const p of pages) {
      const detail = await p.request({ op: "pull", repo, number: 1 })
      expect(detail.pull).toMatchObject({ state: "merged", merge_oid: merged.pull.merge_oid })
      const said = detail.events.filter((e) => e.kind === "comment").map((e) => e.body)
      expect(said.slice().sort()).toEqual(comments)
      expect(detail.events.at(-1)?.kind).toBe("merged")
      expect((await p.request({ op: "pulls", repo, closed: true })).counts).toEqual({ open: 0, closed: 1 })
    }
  })
  for (const server of cluster().servers) {
    const clone = await Work.clone(remote.on(server))
    expect(await clone.head()).toBe(merged.pull.merge_oid!)
    expect(await clone.git("rev-parse", "HEAD^2")).toBe(topic)
  }
})

// Known to fail with several servers: the server that opened the pull
// request writes its ref a moment later, and a merge through another
// server meanwhile answers "atomic push rejected". Each server orders the
// two only among its own work.
;(cluster().servers.length > 1 ? test.failing : test)("a pull request merges at once through another server than it was opened on", async () => {
  const servers = cluster().servers
  for (let round = 0; round < 4; round++) {
    const { owner, repo, topic } = await withTopic()
    const session = await Session.signIn(owner)
    const [opener, merger] = [await session.live(servers[round % servers.length]), await session.live(servers[(round + 1) % servers.length])]
    await opener.request({ op: "pull_create", repo, source: "topic", target: "main", title: "Topic" })
    const merged = await merger.request({ op: "pull_merge", repo, number: 1, head: topic, request: uuid() })
    expect(merged.pull.state).toBe("merged")
    opener.close()
    merger.close()
  }
})

test("pages on every server hear of each change to a pull request", async () => {
  const { owner, repo, topic } = await withTopic()
  await onEveryServer(owner, async (pages) => {
    for (const p of pages) await p.watch(repo)
    const [first] = pages
    await first.request({ op: "pull_create", repo, source: "topic", target: "main", title: "Topic" })
    // Each change through another server.
    const changes = [
      (p: Live) => p.request({ op: "pull_comment", repo, number: 1, body: "A comment" }),
      (p: Live) => p.request({ op: "pull_close", repo, number: 1 }),
      (p: Live) => p.request({ op: "pull_reopen", repo, number: 1 }),
      (p: Live) => p.request({ op: "pull_merge", repo, number: 1, head: topic, request: uuid() }),
    ]
    let version = 0
    for (const [i, change] of changes.entries()) {
      const { pull } = await change(pages[i % pages.length])
      expect(pull.version).toBeGreaterThan(version)
      version = pull.version
      for (const p of pages) {
        await eventually(async () => {
          expect((await p.event(pullChanged(repo, 1))).version).toBeGreaterThanOrEqual(version)
        })
      }
    }
  })
})

test("a push to the source moves the pull request on every server", async () => {
  const { owner, repo, remote, work } = await withTopic()
  const mine = await page(owner)
  await mine.request({ op: "pull_create", repo, source: "topic", target: "main", title: "Topic" })

  await work.git("checkout", "-q", "topic")
  for (const server of cluster().servers) {
    const next = await work.commit(`Through server ${server.index}`)
    await work.push(remote.on(server), "topic")
    await onEveryServer(owner, async (pages) => {
      for (const p of pages) {
        await eventually(async () => {
          const detail = await p.request({ op: "pull", repo, number: 1 })
          expect(detail.pull.head_oid).toBe(next)
          expect(detail.events.at(-1)).toMatchObject({ kind: "push", oid: next })
        })
      }
    })
    for (const reader of cluster().servers) {
      await eventually(async () => expect((await remote.on(reader).refs())["refs/pull/1/head"]).toBe(next))
    }
  }
  // A page that still shows an older commit does not merge the newer one.
  const stale = await work.head("HEAD~1")
  expect(await mine.refused({ op: "pull_merge", repo, number: 1, head: stale, request: uuid() })).toBe("stale")
  mine.close()
})

test("a push that brings the source into the target merges the pull request", async () => {
  const { owner, repo, remote, work, topic } = await withTopic()
  const mine = await page(owner)
  await mine.request({ op: "pull_create", repo, source: "topic", target: "main", title: "Topic" })
  mine.close()
  await work.git("merge", "-q", "--ff-only", "topic")
  await work.push(remote, "main")
  await onEveryServer(owner, async (pages) => {
    for (const p of pages) {
      await eventually(async () => {
        const { pull } = await p.request({ op: "pull", repo, number: 1 })
        expect(pull).toMatchObject({ state: "merged", merge_oid: topic })
      })
    }
  })
})

test("conflicting branches do not merge", async () => {
  const { owner, repo, remote, work, topic } = await withTopic({ "a.txt": "topic\n" })
  await work.commit("On main", { "a.txt": "main\n" })
  await work.push(remote, "main")
  const mine = await page(owner)
  await mine.request({ op: "pull_create", repo, source: "topic", target: "main", title: "Topic" })
  mine.close()
  await onEveryServer(owner, async (pages) => {
    for (const p of pages) {
      const detail = await p.request({ op: "pull", repo, number: 1 })
      expect(detail).toMatchObject({ mergeable: "conflict", conflicts: ["a.txt"] })
      expect(await p.refused({ op: "pull_merge", repo, number: 1, head: topic, request: uuid() })).toBe("conflict")
    }
  })
})

test("a squash merge makes one commit of the branch", async () => {
  const { owner, repo, remote, work, head } = await withTopic()
  await work.git("checkout", "-q", "topic")
  const topic = await work.commit("More on the topic", { "more.txt": "more\n" })
  await work.push(remote, "topic")
  const mine = await page(owner)
  await mine.request({ op: "pull_create", repo, source: "topic", target: "main", title: "Topic" })
  const merged = await mine.request({ op: "pull_merge", repo, number: 1, head: topic, squash: true, request: uuid() })
  mine.close()
  for (const server of cluster().servers) {
    const clone = await Work.clone(remote.on(server))
    expect(await clone.head()).toBe(merged.pull.merge_oid!)
    expect(await clone.git("rev-list", "--parents", "-1", "HEAD")).toBe(`${merged.pull.merge_oid} ${head}`)
    expect(await clone.git("ls-tree", "--name-only", "HEAD")).toBe("a.txt\nmore.txt\ntopic.txt")
  }
})

test("a request sent again through another server changes nothing twice", async () => {
  const { owner, repo, topic } = await withTopic()
  await onEveryServer(owner, async (pages) => {
    const create = { op: "pull_create", repo, source: "topic", target: "main", title: "Topic", request: uuid() } as const
    const numbers = []
    for (const p of pages) numbers.push((await p.request(create)).pull.number)
    expect(numbers).toEqual(pages.map(() => 1))

    const comment = { op: "pull_comment", repo, number: 1, body: "Once", request: uuid() } as const
    for (const p of pages) await p.request(comment)
    const merge = { op: "pull_merge", repo, number: 1, head: topic, request: uuid() } as const
    const merges = []
    for (const p of pages) merges.push((await p.request(merge)).pull.merge_oid)
    expect(new Set(merges).size).toBe(1)

    const detail = await pages[0].request({ op: "pull", repo, number: 1 })
    expect(detail.events.filter((e) => e.kind === "comment")).toHaveLength(1)
    expect(detail.events.filter((e) => e.kind === "merged")).toHaveLength(1)
    expect((await pages[0].request({ op: "pulls", repo, closed: true })).counts).toEqual({ open: 0, closed: 1 })
  })
})

test("pull requests opened at once through different servers get their own numbers", async () => {
  const { owner, repo, remote, work } = await withTopic()
  const branches = ["a", "b", "c", "d", "e", "f"]
  for (const b of branches) {
    await work.git("checkout", "-q", "-b", b, "main")
    await work.commit(`On ${b}`, { [`${b}.txt`]: "x\n" })
  }
  await work.push(remote, ...branches)
  await onEveryServer(owner, async (pages) => {
    const opened = await Promise.all(
      branches.map((source, i) => pages[i % pages.length].request({ op: "pull_create", repo, source, target: "main", title: source }))
    )
    expect(opened.map((o) => o.pull.number).sort()).toEqual([1, 2, 3, 4, 5, 6])
    for (const p of pages) {
      expect((await p.request({ op: "pulls", repo })).counts).toEqual({ open: 6, closed: 0 })
    }
  })
})

test("merges of one pull request at once through different servers make one merge", async () => {
  for (let round = 0; round < 3; round++) {
    const { owner, repo, remote, topic } = await withTopic()
    await onEveryServer(owner, async (pages) => {
      await pages[0].request({ op: "pull_create", repo, source: "topic", target: "main", title: "Topic" })
      // Two pages per server, each with its own request.
      const results = await Promise.all(
        [...pages, ...pages].map((p) =>
          p.request({ op: "pull_merge", repo, number: 1, head: topic, request: uuid() }).then(
            (r) => r.pull.merge_oid,
            (e: Error) => e
          )
        )
      )
      const merges = results.filter((r) => typeof r === "string")
      expect(new Set(merges).size).toBe(1)
      const detail = await pages[0].request({ op: "pull", repo, number: 1 })
      expect(detail.pull).toMatchObject({ state: "merged", merge_oid: merges[0] })
      expect(detail.events.filter((e) => e.kind === "merged")).toHaveLength(1)
      for (const server of cluster().servers) {
        const clone = await Work.clone(remote.on(server))
        expect(await clone.head()).toBe(merges[0] as string)
        // One merge commit on top of the first commit.
        expect(await clone.git("rev-list", "--count", "--first-parent", "HEAD")).toBe("2")
      }
    })
  }
})

test("merges of different pull requests at once all arrive", async () => {
  const { owner, repo, remote, work } = await withTopic()
  const branches = ["a", "b", "c", "d"]
  const heads: string[] = []
  for (const b of branches) {
    await work.git("checkout", "-q", "-b", b, "main")
    heads.push(await work.commit(`On ${b}`, { [`branch-${b}.txt`]: "x\n" }))
  }
  await work.push(remote, ...branches)
  await onEveryServer(owner, async (pages) => {
    for (const source of branches) await pages[0].request({ op: "pull_create", repo, source, target: "main", title: source })
    const results = await Promise.all(
      branches.map((_, i) =>
        pages[i % pages.length].request({ op: "pull_merge", repo, number: i + 1, head: heads[i], request: uuid() }).then(
          () => "merged",
          (e: Error) => e.message
        )
      )
    )
    // A merge that lost against another may ask to be tried again; none is half done.
    for (const [i, result] of results.entries()) {
      if (result !== "merged") {
        await eventually(() => pages[i % pages.length].request({ op: "pull_merge", repo, number: i + 1, head: heads[i], request: uuid() }))
      }
    }
    expect((await pages[0].request({ op: "pulls", repo, closed: true })).counts).toEqual({ open: 0, closed: 4 })
  })
  for (const server of cluster().servers) {
    const clone = await Work.clone(remote.on(server))
    const files = (await clone.git("ls-tree", "--name-only", "HEAD")).split("\n")
    expect(files).toEqual(["a.txt", ...branches.map((b) => `branch-${b}.txt`)])
  }
})

test("a profile counts the pull requests its user opened", async () => {
  const { owner, repo } = await withTopic()
  const mine = await page(owner)
  const before = (await mine.request({ op: "profile", user: owner.name })).totals.lager
  await mine.request({ op: "pull_create", repo, source: "topic", target: "main", title: "Topic" })
  mine.close()
  await onEveryServer(owner, async (pages) => {
    for (const p of pages) {
      await eventually(async () => {
        expect((await p.request({ op: "profile", user: owner.name })).totals.lager).toBe(before + 1)
      })
    }
  })
})
