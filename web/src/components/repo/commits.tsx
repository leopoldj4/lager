import { Link } from "@tanstack/react-router"
import { useEffect, useRef, useState } from "react"

import { useLog } from "@/lib/repos"
import type { Commit } from "@/lib/wire"
import { fullDate, short, since } from "@/lib/format"
import { href } from "@/lib/paths"
import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import { Avatar } from "./avatar"
import { AuthorName } from "./author-name"
import { PageTitle, SectionHead, rowList } from "@/components/frame"

/** The history of a revision, or of a path in it, grouped by day. */
export function CommitList({
  repo,
  rev,
  path,
}: {
  repo: string
  rev: string
  path: string
}) {
  const log = useLog(repo, rev, path)
  const commits = log.data?.pages.flatMap((p) => p.commits)
  const fresh = useFresh(commits)
  const days = groupByDay(commits ?? [])

  return (
    <div className="grid grid-cols-1 gap-8">
      <PageTitle note={path || undefined}>Commits</PageTitle>
      {log.error ? (
        <QueryError error={log.error} />
      ) : !commits ? (
        <Skeleton className="h-96" />
      ) : commits.length === 0 ? (
        <p className="text-sm text-muted-foreground">No commits.</p>
      ) : (
        <div className="grid grid-cols-1 gap-8">
          {days.map(([day, list]) => (
            <section key={day}>
              <SectionHead title={day} count={list.length} />
              <ul className={rowList}>
                {list.map((c) => (
                  <li
                    key={c.oid}
                    className={cn(
                      "flex h-11 items-center gap-3 px-1 transition-colors duration-1000",
                      fresh.has(c.oid) && "animate-in bg-tint fade-in"
                    )}
                  >
                    <Link
                      to="/$"
                      params={{ _splat: href(repo, "tree", c.oid) }}
                      className="w-16 shrink-0 font-mono text-[11px] text-label hover:text-ink"
                      title="Browse files at this commit"
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
                    <Avatar
                      name={c.author.name}
                      className="size-4.5 text-[8px]"
                    />
                    <AuthorName
                      author={c.author}
                      className="hidden w-28 shrink-0 truncate text-xs text-muted-foreground sm:inline"
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
            </section>
          ))}
          {log.hasNextPage && (
            <Button
              variant="outline"
              size="sm"
              className="justify-self-center"
              disabled={log.isFetchingNextPage}
              onClick={() => log.fetchNextPage()}
            >
              {log.isFetchingNextPage ? "Loading…" : "Older commits"}
            </Button>
          )}
        </div>
      )}
    </div>
  )
}

/**
 * Commits above the newest one shown before: a push brought them. Loading
 * older pages adds commits below, which are not new.
 */
function useFresh(commits: Commit[] | undefined) {
  const newest = useRef<string | null>(null)
  const [fresh, setFresh] = useState<Set<string>>(() => new Set())
  const first = commits?.[0]?.oid
  useEffect(() => {
    if (!commits || !first) return
    const before = newest.current
    newest.current = first
    if (!before || before === first) return
    const i = commits.findIndex((c) => c.oid === before)
    // A rewritten branch: nothing to compare with.
    if (i <= 0) return
    setFresh(new Set(commits.slice(0, i).map((c) => c.oid)))
    const t = setTimeout(() => setFresh(new Set()), 8000)
    return () => clearTimeout(t)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [first])
  return fresh
}

function groupByDay(commits: Commit[]): [string, Commit[]][] {
  const out: [string, Commit[]][] = []
  for (const c of commits) {
    const day = new Date(c.committer.time * 1000).toLocaleDateString(
      undefined,
      {
        year: "numeric",
        month: "long",
        day: "numeric",
      }
    )
    const last = out[out.length - 1]
    if (last?.[0] === day) last[1].push(c)
    else out.push([day, [c]])
  }
  return out
}
