import { Link } from "@tanstack/react-router"
import { useEffect, useRef, useState } from "react"

import { useLog, type Commit, type Refs } from "@/lib/api"
import { ago, fullDate, short } from "@/lib/format"
import { href } from "@/lib/paths"
import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import { Avatar } from "./avatar"
import { PathCrumbs } from "./path-crumbs"
import { RevPicker } from "./rev-picker"

export function CommitList({
  repo,
  rev,
  path,
  refs,
}: {
  repo: string
  rev: string
  path: string
  refs: Refs
}) {
  const log = useLog(repo, rev, path)
  const commits = log.data?.pages.flatMap((p) => p.commits)
  const fresh = useFresh(commits)
  const days = groupByDay(commits ?? [])

  return (
    <div className="grid grid-cols-1 gap-4">
      <div className="flex flex-wrap items-center gap-3">
        <RevPicker
          repo={repo}
          view="commits"
          rev={rev}
          path={path}
          refs={refs}
        />
        {path && <PathCrumbs repo={repo} rev={rev} path={path} />}
      </div>
      {log.error ? (
        <QueryError error={log.error} />
      ) : !commits ? (
        <Skeleton className="h-96" />
      ) : commits.length === 0 ? (
        <p className="text-sm text-muted-foreground">No commits.</p>
      ) : (
        <div className="grid grid-cols-1 gap-6">
          {days.map(([day, list]) => (
            <section key={day} className="grid grid-cols-1 gap-2">
              <h2 className="text-sm text-muted-foreground">
                Commits on {day}
              </h2>
              <ul className="divide-y rounded-lg border">
                {list.map((c) => (
                  <li
                    key={c.oid}
                    className={cn(
                      "flex items-center gap-3 px-4 py-3 transition-colors duration-1000",
                      fresh.has(c.oid) && "animate-in bg-emerald-500/10 fade-in"
                    )}
                  >
                    <Avatar name={c.author.name} />
                    <div className="min-w-0 flex-1">
                      <Link
                        to="/$"
                        params={{ _splat: href(repo, "commit", c.oid) }}
                        className="block truncate font-medium hover:underline"
                      >
                        {c.subject}
                      </Link>
                      <p className="text-xs text-muted-foreground">
                        {c.author.name} committed{" "}
                        <span title={fullDate(c.committer.time)}>
                          {ago(c.committer.time)}
                        </span>
                      </p>
                    </div>
                    <Link
                      to="/$"
                      params={{ _splat: href(repo, "tree", c.oid) }}
                      className="rounded-md border px-2 py-1 font-mono text-xs hover:bg-muted"
                      title="Browse files at this commit"
                    >
                      {short(c.oid)}
                    </Link>
                  </li>
                ))}
              </ul>
            </section>
          ))}
          {log.hasNextPage && (
            <Button
              variant="outline"
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
