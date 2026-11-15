// Outages around pull requests and pages: a server that stops or loses
// NATS leaves nothing half done for the others.

import { afterEach, expect, test } from "bun:test"
import { cluster, type Server } from "../harness/cluster"
import { Remote, Work } from "../harness/git"
import { changed, type Event } from "../harness/live"
import { eventually } from "../harness/process"
import { Session } from "../harness/session"
import { newRepo } from "../harness/world"

afterEach(() => cluster().heal())

const several = cluster().servers.length > 1
const uuid = () => crypto.randomUUID()

/** A repository with an open pull request 1 from `topic` into main, opened through `server`. */
async function withPull(server?: Server) {
  const made = await newRepo()
  const remote = new Remote(made.repo, made.owner)
  await made.work.git("checkout", "-q", "-b", "topic")
  const topic = await made.work.commit("On the topic", { "topic.txt": "topic\n" })
  await made.work.push(remote, "topic")
  const session = await Session.signIn(made.owner)
  const page = await session.live(server)
  await page.request({ op: "pull_create", repo: made.repo, source: "topic", target: "main", title: "Topic" })
  page.close()
  return { ...made, remote, topic, session }
}

test("a server that stops during a merge leaves the pull request merged or open, never between", async () => {
  const [victim, ...others] = cluster().servers
  // How long a whole merge takes.
  const first = await withPull(victim)
  const timed = await first.session.live(victim)
  const start = performance.now()
  await timed.request({ op: "pull_merge", repo: first.repo, number: 1, head: first.topic, request: uuid() })
  const whole = performance.now() - start
  timed.close()

  for (const part of [0.2, 0.5, 0.8, 0.9, 1, 1.1, 1.3]) {
    const { repo, remote, topic, head, session } = await withPull(victim)
    const request = uuid()
    const page = await session.live(victim)
    const merge = page.request({ op: "pull_merge", repo, number: 1, head: topic, request }).then(
      (r) => r.pull.merge_oid,
      () => undefined
    )
    await Bun.sleep(whole * part)
    await victim.kill()
    const answered = await merge
    await victim.start()

    for (const reader of [...others, victim]) {
      const main = (await remote.on(reader).refs())["refs/heads/main"]
      const asked = await session.live(reader)
      // A merge that the log has and the database lacks is recorded when a page asks.
      await eventually(async () => {
        const { pull } = await asked.request({ op: "pull", repo, number: 1 })
        if (main === head) {
          expect(pull).toMatchObject({ state: "open", merge_oid: null })
        } else {
          expect(pull).toMatchObject({ state: "merged", merge_oid: main })
        }
      })
      asked.close()
      if (answered) expect(main).toBe(answered)
    }

    // The same request again, through another server, ends merged once.
    const again = await session.live()
    const merged = await again.request({ op: "pull_merge", repo, number: 1, head: topic, request })
    expect(merged.pull.state).toBe("merged")
    const detail = await again.request({ op: "pull", repo, number: 1 })
    expect(detail.events.filter((e) => e.kind === "merged")).toHaveLength(1)
    again.close()
    const clone = await Work.clone(remote)
    expect(await clone.head()).toBe(merged.pull.merge_oid!)
    expect(await clone.git("rev-list", "--count", "--first-parent", "HEAD")).toBe("2")
  }
})

test("a push through a server that then stops still moves the pull request for a page", async () => {
  const { repo, remote, work, session } = await withPull()
  const [victim] = cluster().servers
  const next = await work.commit("More on the topic")
  await work.push(remote.on(victim), "topic")
  await victim.kill()
  if (!several) await victim.start()

  const page = await session.live(cluster().servers.at(-1))
  await eventually(async () => {
    const detail = await page.request({ op: "pull", repo, number: 1 })
    expect(detail.source_oid).toBe(next)
    expect(detail.pull.head_oid).toBe(next)
  })
  page.close()
})

test("the pages of a server that stops go on at another server", async () => {
  const { owner, repo, remote, work, session } = await withPull()
  const [victim] = cluster().servers
  const page = await session.live(victim)
  await page.watch(repo)
  await victim.kill()
  await eventually(() => expect(page.closed).toBeDefined())
  if (!several) await victim.start()

  const moved = await session.live(cluster().servers.at(-1))
  expect(moved.user).toBe(owner.name)
  const before = await moved.watch(repo)
  await work.commit("More on the topic")
  await work.push(remote.on(cluster().servers.at(-1)!), "topic")
  expect((await moved.event(changed(repo))).generation).toBeGreaterThan(before)
  moved.close()
})

// A server that was cut off from NATS missed the events of the others. Its
// pages must hear that events can be missing (`resync`), or they show the
// old state until the next change.
//
// Known to fail: the server sends no `resync` after it finds NATS again.
;(several ? test.failing : test.skip)("pages of a server that was without NATS are told to fetch again", async () => {
  const { repo, remote, work, session } = await withPull()
  const [apart, other] = cluster().servers
  const page = await session.live(apart)
  await page.watch(repo)
  const witness = await session.live(other)
  await witness.watch(repo)

  apart.links.nats.cut()
  const head = await work.commit("While apart")
  await work.push(remote.on(other), "topic")
  // The push and the move of the pull request's ref are both announced.
  await eventually(async () => {
    expect((await witness.request({ op: "pull", repo, number: 1 })).pull.head_oid).toBe(head)
  })
  await Bun.sleep(1000)
  const { generation } = await witness.request({ op: "refs", repo })
  witness.close()
  apart.links.nats.restore()
  await eventually(async () => expect(await apart.healthy()).toBe(true), 30_000)

  // Events from before the outage do not count.
  const news = (e: Event): e is Event => e.type === "resync" || (changed(repo)(e) && e.generation >= generation)
  await page.event(news, 8_000)
  const refs = await page.request({ op: "refs", repo })
  expect(refs.branches.find((b) => b.name === "topic")?.oid).toBe(head)
  page.close()
})
