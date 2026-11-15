import { Link } from "@tanstack/react-router"

import { usePulls } from "@/lib/pulls"
import type { Pull, PullState } from "@/lib/wire"
import type { Refs } from "@/lib/wire"
import { fullDate, since } from "@/lib/format"
import { href } from "@/lib/paths"
import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import { EmptyRow, rowList } from "@/components/frame"
import { Avatar } from "./avatar"

/**
 * How far along a pull request is, on a track of four bars: proposed,
 * pushed, being checked, merged. Done bars are ink, the current one blue.
 */
const stageOf: Record<PullState, number> = { open: 2, merged: 4, closed: -1 }

export const stateLabel: Record<PullState, string> = {
  open: "Open",
  merged: "Merged",
  closed: "Closed",
}

export function StageTrack({
  state,
  className,
}: {
  state: PullState
  className?: string
}) {
  const stage = stageOf[state]
  return (
    <span
      className={cn("flex shrink-0 items-center gap-0.5", className)}
      title={stateLabel[state]}
      role="img"
      aria-label={stateLabel[state]}
    >
      {[0, 1, 2, 3].map((i) => (
        <span
          key={i}
          className={cn(
            "h-3 w-1.5 rounded-[2px]",
            i < stage ? "bg-body" : i === stage ? "bg-primary" : "bg-stage-off"
          )}
        />
      ))}
    </span>
  )
}

/** The state mark of a pull request in lists and search. */
export function PullIcon({
  state,
  className,
}: {
  state: PullState
  className?: string
}) {
  return <StageTrack state={state} className={className} />
}

/** The state as a chip: the track and its name, tinted while open. */
export function PullStateBadge({ state }: { state: PullState }) {
  return (
    <span
      className={cn(
        "inline-flex h-6 items-center gap-2 rounded-[5px] px-2 text-xs font-medium",
        state === "open" ? "bg-tint text-tint-foreground" : "bg-muted text-body"
      )}
    >
      <StageTrack state={state} />
      {stateLabel[state]}
    </span>
  )
}

/** A branch name in mono. */
export function BranchName({ name }: { name: string }) {
  return (
    <span className="rounded-[4px] bg-muted px-1.5 py-0.5 font-mono text-[11px] text-body">
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
    <div className="grid grid-cols-1 gap-6">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-2">
        <h1 className="font-display text-[34px] leading-9 text-ink">
          Pull requests
        </h1>
        {/* Open and closed are two lists of one page, not tabs. */}
        <div className="flex items-baseline gap-3 text-sm">
          {([false, true] as const).map((c) => (
            <Link
              key={String(c)}
              to="/$"
              params={{
                _splat: c ? href(repo, "pulls", "closed") : href(repo, "pulls"),
              }}
              className={cn(
                c === closed
                  ? "font-medium text-ink"
                  : "text-muted-foreground hover:text-ink"
              )}
            >
              {counts && (c ? counts.closed : counts.open)}{" "}
              {c ? "closed" : "open"}
            </Link>
          ))}
        </div>
      </div>
      <div>
        <div className="flex items-center gap-2 pb-2">
          <h2 className="label-caps">{closed ? "Closed" : "Open"}</h2>
          {counts && (
            <span className="font-mono text-[11px] font-medium text-label">
              {closed ? counts.closed : counts.open}
            </span>
          )}
        </div>
        {q.error ? (
          <QueryError error={q.error} />
        ) : !pulls ? (
          <Skeleton className="h-48" />
        ) : pulls.length === 0 ? (
          <EmptyRow>
            {closed
              ? "No closed pull requests."
              : refs.mirror
                ? `No open pull requests on github.com/${refs.mirror}.`
                : "No open pull requests. Push a branch and open one to propose its changes."}
          </EmptyRow>
        ) : (
          <ul className={rowList}>
            {pulls.map((p) => (
              <PullRow key={p.number} repo={repo} pull={p} />
            ))}
          </ul>
        )}
      </div>
      {q.hasNextPage && (
        <Button
          variant="outline"
          size="sm"
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

/**
 * One pull request in a list: state, number, title, source branch,
 * author, and age, in fixed slots so columns align across rows.
 */
export function PullRow({ repo, pull: p }: { repo: string; pull: Pull }) {
  const time = p.state === "open" ? p.created_ms : (p.closed_ms ?? p.updated_ms)
  return (
    <li>
      <Link
        to="/$"
        params={{ _splat: href(repo, "pull", String(p.number)) }}
        className="flex h-11 items-center gap-3.5 px-1 hover:bg-muted/50"
      >
        <StageTrack state={p.state} className="w-7.5" />
        <span className="w-10 shrink-0 font-mono text-[11px] font-medium text-[#55555D] dark:text-muted-foreground">
          #{p.number}
        </span>
        <span className="min-w-0 truncate text-[13px] font-medium text-ink">
          {p.title}
        </span>
        <span className="ml-auto hidden max-w-48 shrink-0 truncate font-mono text-[11px] text-label md:inline">
          {p.source}
        </span>
        <Avatar name={p.author} className="size-5 text-[9px] max-md:ml-auto" />
        <span
          className="w-7 shrink-0 text-right text-xs text-label"
          title={`${p.state === "open" ? "Opened" : stateLabel[p.state]} ${fullDate(time / 1000)} by ${p.author}`}
        >
          {since(time / 1000)}
        </span>
      </Link>
    </li>
  )
}
