import { Link } from "@tanstack/react-router"
import {
  GitMerge,
  GitPullRequest,
  GitPullRequestClosed,
  Plus,
} from "lucide-react"

import { usePulls, type PullState, type Refs } from "@/lib/api"
import { ago } from "@/lib/format"
import { href } from "@/lib/paths"
import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"

const stateStyle: Record<PullState, { icon: typeof GitMerge; color: string }> =
  {
    open: { icon: GitPullRequest, color: "text-emerald-600" },
    merged: { icon: GitMerge, color: "text-violet-600" },
    closed: { icon: GitPullRequestClosed, color: "text-red-600" },
  }

export function PullIcon({
  state,
  className,
}: {
  state: PullState
  className?: string
}) {
  const { icon: Icon, color } = stateStyle[state]
  return <Icon className={cn("size-4 shrink-0", color, className)} />
}

export function PullStateBadge({ state }: { state: PullState }) {
  const { icon: Icon } = stateStyle[state]
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full px-3 py-1 text-sm font-medium text-white capitalize",
        state === "open" && "bg-emerald-600",
        state === "merged" && "bg-violet-600",
        state === "closed" && "bg-red-600"
      )}
    >
      <Icon className="size-4" />
      {state}
    </span>
  )
}

/** A branch name in a pill. */
export function BranchName({ name }: { name: string }) {
  return (
    <span className="rounded-md bg-muted px-1.5 py-0.5 font-mono text-xs">
      {name}
    </span>
  )
}

/** The page that opens a pull request: the default branch and another. */
export function newPullHref(repo: string, refs: Refs) {
  const other = [...refs.branches]
    .sort((a, b) => b.time - a.time)
    .find((b) => b.name !== refs.head)
  return href(repo, "compare", `${refs.head}...${other?.name ?? refs.head}`)
}

export function PullList({
  repo,
  closed,
  refs,
}: {
  repo: string
  closed: boolean
  refs: Refs
}) {
  const q = usePulls(repo, closed)
  const pulls = q.data?.pages.flatMap((p) => p.pulls)
  const counts = q.data?.pages[0]?.counts

  return (
    <div className="grid grid-cols-1 gap-4">
      <div className="flex flex-wrap items-center gap-3">
        <div className="flex gap-1">
          {([false, true] as const).map((c) => (
            <Link
              key={String(c)}
              to="/$"
              params={{
                _splat: c ? href(repo, "pulls", "closed") : href(repo, "pulls"),
              }}
              className={cn(
                "flex items-center gap-1.5 rounded-md px-2.5 py-1 text-sm",
                c === closed
                  ? "bg-muted font-medium"
                  : "text-muted-foreground hover:text-foreground"
              )}
            >
              {c ? (
                <GitPullRequestClosed className="size-4" />
              ) : (
                <GitPullRequest className="size-4" />
              )}
              {counts && (c ? counts.closed : counts.open)}{" "}
              {c ? "Closed" : "Open"}
            </Link>
          ))}
        </div>
        {refs.branches.length > 1 && (
          <Button asChild size="sm" className="ml-auto">
            <Link to="/$" params={{ _splat: newPullHref(repo, refs) }}>
              <Plus />
              New pull request
            </Link>
          </Button>
        )}
      </div>
      {q.error ? (
        <QueryError error={q.error} />
      ) : !pulls ? (
        <Skeleton className="h-48" />
      ) : pulls.length === 0 ? (
        <p className="rounded-lg border border-dashed p-10 text-center text-sm text-muted-foreground">
          {closed
            ? "No closed pull requests."
            : "No open pull requests. Push a branch and open one to propose its changes."}
        </p>
      ) : (
        <ul className="divide-y rounded-lg border">
          {pulls.map((p) => (
            <li key={p.number} className="flex items-start gap-3 px-4 py-3">
              <PullIcon state={p.state} className="mt-0.5" />
              <div className="min-w-0 flex-1">
                <Link
                  to="/$"
                  params={{ _splat: href(repo, "pull", String(p.number)) }}
                  className="font-medium break-words hover:underline"
                >
                  {p.title}
                </Link>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  #{p.number}{" "}
                  {p.state === "merged"
                    ? `merged ${ago((p.closed_ms ?? p.updated_ms) / 1000)}`
                    : p.state === "closed"
                      ? `closed ${ago((p.closed_ms ?? p.updated_ms) / 1000)}`
                      : `opened ${ago(p.created_ms / 1000)}`}{" "}
                  by {p.author}
                </p>
              </div>
              <span className="hidden shrink-0 items-center gap-1 text-xs text-muted-foreground sm:flex">
                <BranchName name={p.source} />→<BranchName name={p.target} />
              </span>
            </li>
          ))}
        </ul>
      )}
      {q.hasNextPage && (
        <Button
          variant="outline"
          className="justify-self-center"
          disabled={q.isFetchingNextPage}
          onClick={() => q.fetchNextPage()}
        >
          {q.isFetchingNextPage ? "Loading…" : "More"}
        </Button>
      )}
    </div>
  )
}
