import { Link } from "@tanstack/react-router"
import { Clock, GitBranch, GitPullRequestCreate } from "lucide-react"
import { useEffect } from "react"

import { usePulls } from "@/lib/pulls"
import type { Pull } from "@/lib/wire"
import { useLive } from "@/lib/query"
import { useLog, useTree } from "@/lib/repos"
import type { Commit, Refs } from "@/lib/wire"
import { fullDate, short, since } from "@/lib/format"
import { href } from "@/lib/paths"
import { cn } from "@/lib/utils"
import { Badge } from "@/components/ui/badge"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import { GithubMark } from "@/components/github-mark"
import { EmptyRow, SectionHead, rowList } from "@/components/frame"
import { Avatar, repoColor } from "./avatar"
import { AuthorName } from "./author-name"
import { CloneButton } from "./clone"
import { PullRow } from "./pulls"
import { findReadme, Readme } from "./tree"

/** Pull requests and commits shown in a section before "All". */
const SHOWN = 5
/** The days the commit count and its bars cover. */
const DAYS = 30
/** Pages of history read at most for the count: 30 commits each. */
const MAX_PAGES = 10

/**
 * The first page of a repository: who it is, a strip of numbers, then
 * what is in progress and what changed. The code is one link away.
 */
export function Overview({ repo, refs }: { repo: string; refs: Refs }) {
  const user = useLive()?.user
  const pulls = usePulls(repo, false)
  const open = pulls.data?.pages.flatMap((p) => p.pulls)
  // Authors of pull requests from GitHub are GitHub logins.
  const mine =
    (user && open?.filter((p) => !p.github_url && p.author === user)) || []
  const counts = pulls.data?.pages[0]?.counts
  const activity = useActivity(repo, refs.head)

  return (
    <div className="flex min-h-[calc(var(--frame-height)-3rem)] shrink-0 flex-col">
      <Hero repo={repo} refs={refs} updated={activity.commits?.[0]} />
      <div className="grid shrink-0 grid-cols-2 border-y border-hairline lg:grid-cols-4">
        <Stat label="Open pull requests" to={href(repo, "pulls")}>
          <Big>{counts?.open ?? "–"}</Big>
          {mine.length > 0 ? (
            <span className="text-xs font-medium text-tint-foreground">
              {mine.length} yours
            </span>
          ) : (
            counts && (
              <span className="text-xs text-label">{counts.closed} closed</span>
            )
          )}
        </Stat>
        <Stat label="Branches" to={href(repo, "branches")}>
          <Big>{refs.branches.length}</Big>
          <span className="text-xs text-label">
            {refs.tags.length} {refs.tags.length === 1 ? "tag" : "tags"}
          </span>
        </Stat>
        <LatestTag repo={repo} refs={refs} />
        <CommitStat repo={repo} rev={refs.head} activity={activity} />
      </div>
      <div className="flex flex-1 flex-col lg:flex-row">
        <div className="grid min-w-0 flex-1 grid-cols-1 content-start gap-8 px-4 py-7 md:px-10">
          <section>
            <SectionHead
              title="Pull requests"
              count={counts?.open}
              more={{ label: "View all", to: href(repo, "pulls") }}
            />
            {pulls.error ? (
              <QueryError error={pulls.error} />
            ) : !open ? (
              <Skeleton className="h-32" />
            ) : open.length === 0 ? (
              <EmptyRow>No open pull requests.</EmptyRow>
            ) : (
              <ul className={rowList}>
                {/* The user's own come first. */}
                {[...mine, ...open.filter((p) => !mine.includes(p))]
                  .slice(0, SHOWN)
                  .map((p) => (
                    <PullRow key={p.number} repo={repo} pull={p} />
                  ))}
              </ul>
            )}
          </section>
          <RecentCommits
            repo={repo}
            rev={refs.head}
            commits={activity.commits}
            error={activity.error}
          />
          <RootReadme repo={repo} rev={refs.head} />
        </div>
        <aside className="grid shrink-0 grid-cols-1 content-start gap-7 border-t border-hairline px-4 py-7 md:px-6 lg:w-70 lg:border-t-0 lg:border-l">
          <Properties refs={refs} commits={activity.commits} />
          <RecentBranches repo={repo} refs={refs} open={open} />
        </aside>
      </div>
    </div>
  )
}

/**
 * The commits of the default branch, enough pages of them to cover the
 * last DAYS days, up to MAX_PAGES.
 */
function useActivity(repo: string, rev: string) {
  const log = useLog(repo, rev, "")
  const commits = log.data?.pages.flatMap((p) => p.commits)
  const oldest = commits?.[commits.length - 1]
  const from = Date.now() / 1000 - DAYS * 24 * 3600
  const pages = log.data?.pages.length ?? 0
  const more =
    !!oldest &&
    oldest.committer.time >= from &&
    log.hasNextPage &&
    pages < MAX_PAGES
  useEffect(() => {
    if (more && !log.isFetchingNextPage) log.fetchNextPage()
  }, [more, log])
  const recent = commits?.filter((c) => c.committer.time >= from)
  return {
    commits,
    recent,
    /** More commits in the window than were read. */
    capped: !!oldest && oldest.committer.time >= from && log.hasNextPage,
    loading: more,
    error: log.error,
  }
}

function Hero({
  repo,
  refs,
  updated,
}: {
  repo: string
  refs: Refs
  updated: Commit | undefined
}) {
  const name = repo.split("/").slice(1).join("/")
  const color = repoColor(repo)
  return (
    <div className="flex flex-wrap items-start justify-between gap-4 px-4 pt-8 pb-7 md:px-10">
      <div className="flex min-w-0 items-start gap-4.5">
        {/* The logo tile: the repository's colour, its first letter. */}
        <span
          className="flex size-14 shrink-0 items-center justify-center rounded-[13px] font-display text-[30px] leading-none"
          style={{
            backgroundColor: `color-mix(in oklab, ${color} 14%, transparent)`,
            color,
          }}
          aria-hidden
        >
          {name.slice(0, 1).toUpperCase()}
        </span>
        <div className="grid min-w-0 gap-1.5">
          <div className="flex min-w-0 flex-wrap items-center gap-2.5">
            <h1 className="min-w-0 font-display text-[34px] leading-9 break-all text-ink">
              {name}
            </h1>
            <Badge variant="outline">
              {refs.public ? "Public" : "Private"}
            </Badge>
          </div>
          <div className="flex flex-wrap items-center gap-x-4 gap-y-1 pt-1.5 text-xs text-[#55555D] dark:text-muted-foreground">
            <span className="flex items-center gap-1.5">
              <GitBranch className="size-3 text-label" />
              <span className="font-mono text-[11px]">{refs.head}</span>
            </span>
            {refs.mirror && (
              <a
                href={`https://github.com/${refs.mirror}`}
                target="_blank"
                rel="noreferrer"
                title="Pushes go to GitHub first, and this copy follows"
                className="flex items-center gap-1.5 hover:text-ink"
              >
                <GithubMark className="size-3 text-label" />
                {refs.mirror}
              </a>
            )}
            {updated && (
              <span
                className="flex items-center gap-1.5"
                title={fullDate(updated.committer.time)}
              >
                <Clock className="size-3 text-label" />
                Updated {since(updated.committer.time)} ago
              </span>
            )}
          </div>
        </div>
      </div>
      <CloneButton repo={repo} />
    </div>
  )
}

/** A tile of the strip; with `to`, the whole tile links to that page. */
function Stat({
  label,
  note,
  to,
  children,
}: {
  label: string
  note?: string
  /** The splat of the page the tile opens. */
  to?: string
  children: React.ReactNode
}) {
  const className = cn(
    "flex min-w-0 flex-col gap-2.5 border-hairline px-4 py-4.5 max-lg:odd:border-r md:px-6 lg:not-last:border-r lg:first:pl-10 lg:last:pr-10 max-lg:[&:nth-child(-n+2)]:border-b",
    to && "transition-colors hover:bg-muted/50"
  )
  const body = (
    <>
      <h2 className="truncate label-caps">
        {label}
        {note && <span className="font-normal text-label"> · {note}</span>}
      </h2>
      <div className="flex h-9 min-w-0 items-baseline gap-2">{children}</div>
    </>
  )
  return to ? (
    <Link to="/$" params={{ _splat: to }} className={className}>
      {body}
    </Link>
  ) : (
    <div className={className}>{body}</div>
  )
}

function Big({ children }: { children: React.ReactNode }) {
  return (
    <span className="truncate font-display text-[30px] leading-9 text-ink">
      {children}
    </span>
  )
}

function LatestTag({ repo, refs }: { repo: string; refs: Refs }) {
  const tag = [...refs.tags].sort((a, b) => b.time - a.time)[0]
  return (
    <Stat label="Latest tag">
      {tag ? (
        <>
          <Link
            to="/$"
            params={{ _splat: href(repo, "tree", tag.name) }}
            className="min-w-0 truncate font-display text-[26px] leading-9 text-ink hover:underline"
          >
            {tag.name}
          </Link>
          <span
            className="shrink-0 text-xs text-label"
            title={fullDate(tag.time)}
          >
            {since(tag.time)}
          </span>
        </>
      ) : (
        <Big>–</Big>
      )}
    </Stat>
  )
}

/** Commits of the last DAYS days, with a bar for each day; today's is blue. */
function CommitStat({
  repo,
  rev,
  activity,
}: {
  repo: string
  rev: string
  activity: ReturnType<typeof useActivity>
}) {
  const days = new Array<number>(DAYS).fill(0)
  const today = new Date()
  today.setHours(0, 0, 0, 0)
  for (const c of activity.recent ?? []) {
    const d = new Date(c.committer.time * 1000)
    d.setHours(0, 0, 0, 0)
    const ago = Math.round((today.getTime() - d.getTime()) / 86_400_000)
    if (ago >= 0 && ago < DAYS) days[DAYS - 1 - ago]++
  }
  const max = Math.max(1, ...days)
  return (
    <Stat label="Commits" note={`${DAYS} days`} to={href(repo, "commits", rev)}>
      <span className="flex w-full items-end gap-4 self-end">
        <Big>
          {activity.recent ? activity.recent.length : "–"}
          {activity.capped && !activity.loading && "+"}
        </Big>
        <span
          className="hidden h-7.5 min-w-0 flex-1 items-end gap-0.5 sm:flex"
          aria-hidden
        >
          {days.map((n, i) => (
            <span
              key={i}
              className={cn(
                "min-w-0 flex-1 rounded-[1.5px]",
                i === DAYS - 1 ? "bg-primary" : "bg-chart-1"
              )}
              style={{ height: `${Math.max(10, (n / max) * 100)}%` }}
              title={`${n} ${n === 1 ? "commit" : "commits"}`}
            />
          ))}
        </span>
      </span>
    </Stat>
  )
}

function RecentCommits({
  repo,
  rev,
  commits,
  error,
}: {
  repo: string
  rev: string
  commits: Commit[] | undefined
  error: Error | null
}) {
  return (
    <section>
      <SectionHead
        title="Recent commits"
        note={rev}
        more={{ label: "History", to: href(repo, "commits", rev) }}
      />
      {error ? (
        <QueryError error={error} />
      ) : !commits ? (
        <Skeleton className="h-40" />
      ) : (
        <ul className={rowList}>
          {commits.slice(0, SHOWN).map((c, i) => (
            <li key={c.oid} className="flex h-10 items-center gap-3 px-1">
              <span className="flex w-4 shrink-0 justify-center">
                <span
                  className={cn(
                    "size-[7px] rounded-full",
                    i === 0 ? "bg-primary" : "bg-stage-off"
                  )}
                />
              </span>
              <Link
                to="/$"
                params={{ _splat: href(repo, "commit", c.oid) }}
                className="hidden w-16 shrink-0 font-mono text-[11px] text-label hover:text-ink sm:inline"
              >
                {short(c.oid).slice(0, 7)}
              </Link>
              <Link
                to="/$"
                params={{ _splat: href(repo, "commit", c.oid) }}
                className="min-w-0 flex-1 truncate text-[13px] text-body hover:text-ink"
              >
                {c.subject}
              </Link>
              <Avatar name={c.author.name} className="size-4.5 text-[8px]" />
              <AuthorName
                author={c.author}
                className="hidden w-18 shrink-0 truncate text-xs text-[#55555D] sm:inline dark:text-muted-foreground"
              />
              <span
                className="w-7 shrink-0 text-right text-xs text-label"
                title={fullDate(c.committer.time)}
              >
                {since(c.committer.time)}
              </span>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}

function RootReadme({ repo, rev }: { repo: string; rev: string }) {
  const tree = useTree(repo, rev, "")
  const readme = tree.data && findReadme(tree.data.entries)
  if (!readme) return null
  return <Readme repo={repo} rev={rev} path={readme.name} />
}

/** Label and value rows at the top of the side panel. */
function Properties({
  refs,
  commits,
}: {
  refs: Refs
  commits: Commit[] | undefined
}) {
  // People: the authors of the commits read for the overview.
  const people = [...new Set(commits?.map((c) => c.author.name))]
  return (
    <dl className="grid grid-cols-[6rem_1fr] items-center gap-y-3.5 text-xs">
      <dt className="text-label">Branches</dt>
      <dd className="text-[13px] font-medium text-ink">
        {refs.branches.length}
      </dd>
      <dt className="text-label">Tags</dt>
      <dd className="text-[13px] font-medium text-ink">{refs.tags.length}</dd>
      {people.length > 0 && (
        <>
          <dt className="text-label">People</dt>
          <dd className="flex items-center">
            {people.slice(0, 3).map((p, i) => (
              <span key={p} title={p} className={cn(i > 0 && "-ml-1.5")}>
                <Avatar
                  name={p}
                  className="size-5.5 text-[9px] ring-2 ring-background"
                />
              </span>
            ))}
            {people.length > 3 && (
              <span className="pl-2 text-label">+{people.length - 3}</span>
            )}
          </dd>
        </>
      )}
    </dl>
  )
}

/**
 * Branches pushed most recently, other than the default one: a blue dot
 * marks one with an open pull request, which it links to.
 */
function RecentBranches({
  repo,
  refs,
  open,
}: {
  repo: string
  refs: Refs
  open: Pull[] | undefined
}) {
  const user = useLive()?.user
  const head = refs.branches.find((b) => b.name === refs.head)
  const branches = refs.branches
    .filter((b) => b.name !== refs.head)
    .sort((a, b) => b.time - a.time)
    .slice(0, SHOWN)
  if (branches.length === 0) return null
  return (
    <section className="grid grid-cols-1 gap-2.5 border-t border-hairline pt-6">
      <div className="flex items-center">
        <h2 className="label-caps">Recent branches</h2>
        <Link
          to="/$"
          params={{ _splat: href(repo, "branches") }}
          className="ml-auto text-xs font-medium text-label hover:text-ink"
        >
          All
        </Link>
      </div>
      <ul className="grid grid-cols-1">
        {branches.map((b) => {
          const pull = open?.find((p) => p.source === b.name)
          return (
            <li key={b.name} className="flex h-6.5 items-center gap-2">
              <span
                className={cn(
                  "size-1.5 shrink-0 rounded-full",
                  pull ? "bg-primary" : "bg-stage-off"
                )}
              />
              <Link
                to="/$"
                params={{
                  _splat: pull
                    ? href(repo, "pull", String(pull.number))
                    : href(repo, "tree", b.name),
                }}
                title={pull ? `#${pull.number} ${pull.title}` : b.name}
                className={cn(
                  "min-w-0 flex-1 truncate font-mono text-xs hover:underline",
                  pull
                    ? "text-ink"
                    : "text-[#55555D] dark:text-muted-foreground"
                )}
              >
                {b.name}
              </Link>
              {!pull && user && open && b.oid !== head?.oid && (
                <Link
                  to="/$"
                  params={{
                    _splat: href(repo, "compare", `${refs.head}...${b.name}`),
                  }}
                  title="Open a pull request"
                  aria-label={`Open a pull request from ${b.name}`}
                  className="shrink-0 text-label hover:text-ink"
                >
                  <GitPullRequestCreate className="size-3.5" />
                </Link>
              )}
              <span
                className="shrink-0 text-xs text-label"
                title={fullDate(b.time)}
              >
                {since(b.time)}
              </span>
            </li>
          )
        })}
      </ul>
    </section>
  )
}
