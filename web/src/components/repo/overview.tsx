import { Link } from "@tanstack/react-router"
import { ArrowRight, GitBranch, Plus, Tag } from "lucide-react"

import {
  useLive,
  useLog,
  usePulls,
  useTree,
  type Pull,
  type Refs,
} from "@/lib/api"
import { ago, fullDate, short } from "@/lib/format"
import { href } from "@/lib/paths"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import { Avatar } from "./avatar"
import { newPullHref, PullIcon, PullRow } from "./pulls"
import { findReadme, Readme } from "./tree"

/** Pull requests and commits shown in a section before "All". */
const SHOWN = 5

/**
 * The first page of a repository: what is in progress, then what changed.
 * The code is one tab away.
 */
export function Overview({ repo, refs }: { repo: string; refs: Refs }) {
  const user = useLive()?.user
  const pulls = usePulls(repo, false)
  const open = pulls.data?.pages.flatMap((p) => p.pulls)
  const mine = (user && open?.filter((p) => p.author === user)) || []
  const others = open?.filter((p) => !mine.includes(p))
  const total = pulls.data?.pages[0]?.counts.open

  return (
    <div className="grid grid-cols-1 gap-6 lg:grid-cols-[minmax(0,1fr)_18rem]">
      <div className="grid min-w-0 grid-cols-1 content-start gap-6">
        {mine.length > 0 && (
          <Section title="Your pull requests" count={mine.length}>
            <ul className="divide-y rounded-lg border">
              {mine.map((p) => (
                <PullRow key={p.number} repo={repo} pull={p} />
              ))}
            </ul>
          </Section>
        )}
        {!(mine.length > 0 && others?.length === 0) && (
          <Section
            title={mine.length > 0 ? "Other pull requests" : "Pull requests"}
            count={total === undefined ? undefined : total - mine.length}
            more={{ label: "All", to: href(repo, "pulls") }}
          >
            {pulls.error ? (
              <QueryError error={pulls.error} />
            ) : !others ? (
              <Skeleton className="h-32" />
            ) : others.length === 0 ? (
              <NoPulls repo={repo} refs={refs} />
            ) : (
              <ul className="divide-y rounded-lg border">
                {others.slice(0, SHOWN).map((p) => (
                  <PullRow key={p.number} repo={repo} pull={p} />
                ))}
              </ul>
            )}
          </Section>
        )}
        <RecentCommits repo={repo} rev={refs.head} />
        <RootReadme repo={repo} rev={refs.head} />
      </div>
      <aside className="grid grid-cols-1 content-start gap-6">
        <RecentBranches repo={repo} refs={refs} open={open} />
        <LatestTag repo={repo} refs={refs} />
      </aside>
    </div>
  )
}

function Section({
  title,
  count,
  more,
  children,
}: {
  title: string
  count?: number
  more?: { label: string; to: string }
  children: React.ReactNode
}) {
  return (
    <section className="grid grid-cols-1 gap-2">
      <div className="flex items-center gap-2">
        <h2 className="font-semibold">{title}</h2>
        {count !== undefined && (
          <span className="rounded-full bg-muted px-1.5 text-xs">{count}</span>
        )}
        {more && (
          <Link
            to="/$"
            params={{ _splat: more.to }}
            className="ml-auto flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
          >
            {more.label}
            <ArrowRight className="size-3.5" />
          </Link>
        )}
      </div>
      {children}
    </section>
  )
}

function NoPulls({ repo, refs }: { repo: string; refs: Refs }) {
  return (
    <div className="flex flex-wrap items-center gap-3 rounded-lg border border-dashed px-4 py-6 text-sm text-muted-foreground">
      No open pull requests.
      {refs.branches.length > 1 && (
        <Button asChild size="sm" variant="outline" className="ml-auto">
          <Link to="/$" params={{ _splat: newPullHref(repo, refs) }}>
            <Plus />
            New pull request
          </Link>
        </Button>
      )}
    </div>
  )
}

function RecentCommits({ repo, rev }: { repo: string; rev: string }) {
  const log = useLog(repo, rev, "")
  const commits = log.data?.pages[0]?.commits.slice(0, SHOWN)
  return (
    <Section
      title="Recent commits"
      more={{ label: "History", to: href(repo, "commits", rev) }}
    >
      {log.error ? (
        <QueryError error={log.error} />
      ) : !commits ? (
        <Skeleton className="h-40" />
      ) : (
        <ul className="divide-y rounded-lg border text-sm">
          {commits.map((c) => (
            <li key={c.oid} className="flex items-center gap-3 px-4 py-2.5">
              <Avatar name={c.author.name} className="size-5 text-[9px]" />
              <Link
                to="/$"
                params={{ _splat: href(repo, "commit", c.oid) }}
                className="min-w-0 flex-1 truncate hover:underline"
              >
                {c.subject}
              </Link>
              <span className="hidden shrink-0 text-xs text-muted-foreground sm:inline">
                {c.author.name}
              </span>
              <span
                className="shrink-0 text-xs text-muted-foreground"
                title={fullDate(c.committer.time)}
              >
                {ago(c.committer.time)}
              </span>
              <span className="hidden shrink-0 font-mono text-xs text-muted-foreground sm:inline">
                {short(c.oid)}
              </span>
            </li>
          ))}
        </ul>
      )}
    </Section>
  )
}

function RootReadme({ repo, rev }: { repo: string; rev: string }) {
  const tree = useTree(repo, rev, "")
  const readme = tree.data && findReadme(tree.data.entries)
  if (!readme) return null
  return <Readme repo={repo} rev={rev} path={readme.name} />
}

/**
 * Branches pushed most recently, other than the default one: each links
 * to its pull request, or offers to open one.
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
    <Section
      title="Recent branches"
      more={{ label: "All", to: href(repo, "branches") }}
    >
      <ul className="divide-y rounded-lg border text-sm">
        {branches.map((b) => {
          const pull = open?.find((p) => p.source === b.name)
          return (
            <li key={b.name} className="flex items-center gap-2 px-3 py-2">
              <GitBranch className="size-4 shrink-0 text-muted-foreground" />
              <div className="min-w-0 flex-1">
                <Link
                  to="/$"
                  params={{ _splat: href(repo, "tree", b.name) }}
                  className="block truncate font-mono text-xs hover:underline"
                >
                  {b.name}
                </Link>
                <span
                  className="text-xs text-muted-foreground"
                  title={fullDate(b.time)}
                >
                  {ago(b.time)}
                </span>
              </div>
              {pull ? (
                <Link
                  to="/$"
                  params={{ _splat: href(repo, "pull", String(pull.number)) }}
                  className="flex shrink-0 items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
                >
                  <PullIcon state="open" className="size-3.5" />#{pull.number}
                </Link>
              ) : (
                user &&
                open &&
                b.oid !== head?.oid && (
                  <Button asChild size="xs" variant="outline">
                    <Link
                      to="/$"
                      params={{
                        _splat: href(
                          repo,
                          "compare",
                          `${refs.head}...${b.name}`
                        ),
                      }}
                    >
                      Open pull request
                    </Link>
                  </Button>
                )
              )}
            </li>
          )
        })}
      </ul>
    </Section>
  )
}

function LatestTag({ repo, refs }: { repo: string; refs: Refs }) {
  const tag = [...refs.tags].sort((a, b) => b.time - a.time)[0]
  if (!tag) return null
  return (
    <Section title="Latest tag" more={{ label: "All", to: href(repo, "tags") }}>
      <Link
        to="/$"
        params={{ _splat: href(repo, "tree", tag.name) }}
        className="flex items-center gap-2 rounded-lg border px-3 py-2 text-sm hover:bg-muted/50"
      >
        <Tag className="size-4 text-muted-foreground" />
        <span className="font-mono text-xs font-medium">{tag.name}</span>
        <span
          className="ml-auto text-xs text-muted-foreground"
          title={fullDate(tag.time)}
        >
          {ago(tag.time)}
        </span>
      </Link>
    </Section>
  )
}
