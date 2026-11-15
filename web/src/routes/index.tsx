import { Link, createFileRoute } from "@tanstack/react-router"
import { Search } from "lucide-react"
import { useState } from "react"

import { usePullCounts } from "@/lib/pulls"
import { useLive } from "@/lib/query"
import { useRepos } from "@/lib/repos"
import type { RepoSummary } from "@/lib/wire"
import { href } from "@/lib/paths"
import { Badge } from "@/components/ui/badge"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import { GithubMark } from "@/components/github-mark"
import { CreateRepoDialog } from "@/components/create-repo-dialog"
import { RepoDot } from "@/components/repo/avatar"
import {
  EmptyRow,
  PageBody,
  PageHeader,
  PageTitle,
  rowList,
} from "@/components/frame"

export const Route = createFileRoute("/")({ component: Repositories })

function Repositories() {
  const repos = useRepos()
  const state = useLive()
  const [filter, setFilter] = useState("")
  const shown = repos.data?.repos.filter((r) =>
    r.name.toLowerCase().includes(filter.toLowerCase())
  )

  return (
    <>
      <PageHeader actions={state?.user && <CreateRepoDialog />}>
        <span className="text-[13px] font-medium text-ink">Repositories</span>
      </PageHeader>
      <PageBody className="grid grid-cols-1 gap-6">
        <div className="flex flex-wrap items-end gap-4">
          <PageTitle note={repos.data && `${repos.data.repos.length} in total`}>
            Repositories
          </PageTitle>
          <label className="ml-auto flex h-8 w-full items-center gap-2 rounded-md border px-2.5 focus-within:ring-2 focus-within:ring-ring/30 sm:w-64">
            <Search className="size-3.5 shrink-0 text-label" />
            <input
              placeholder="Find a repository"
              className="min-w-0 flex-1 bg-transparent text-[13px] outline-none placeholder:text-label"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
          </label>
        </div>
        {repos.error ? (
          <QueryError error={repos.error} />
        ) : !shown ? (
          <div className="grid grid-cols-1 gap-2">
            {[0, 1, 2].map((i) => (
              <Skeleton key={i} className="h-10" />
            ))}
          </div>
        ) : (
          <div>
            <div className="flex items-center gap-2 pb-2">
              <h2 className="label-caps">{filter ? "Matching" : "All"}</h2>
              <span className="font-mono text-[11px] font-medium text-label">
                {shown.length}
              </span>
            </div>
            {shown.length === 0 ? (
              <EmptyRow>
                {filter
                  ? "No repository matches."
                  : state?.user
                    ? "No repositories yet. Create one or push to a new name."
                    : "No public repositories. Sign in to see yours."}
              </EmptyRow>
            ) : (
              <ul className={rowList}>
                {shown.map((r) => (
                  <RepoRow key={r.name} repo={r} />
                ))}
              </ul>
            )}
          </div>
        )}
      </PageBody>
    </>
  )
}

function RepoRow({ repo: r }: { repo: RepoSummary }) {
  const open = usePullCounts(r.name).data?.open
  const [owner, ...rest] = r.name.split("/")
  return (
    <li>
      <Link
        to="/$"
        params={{ _splat: href(r.name) }}
        className="flex h-11 items-center gap-3.5 px-1 hover:bg-muted/50"
      >
        <span className="flex w-4 shrink-0 justify-center">
          {r.mirror ? (
            <GithubMark className="size-3 text-label" />
          ) : (
            <RepoDot name={r.name} />
          )}
        </span>
        <span className="min-w-0 truncate text-[13px] font-medium">
          <span className="text-label">{owner} / </span>
          <span className="text-ink">{rest.join("/")}</span>
        </span>
        <Badge variant="outline">
          {r.anonymous_read ? "Public" : "Private"}
        </Badge>
        <span className="ml-auto hidden font-mono text-[11px] text-muted-foreground sm:inline">
          {r.default_branch}
        </span>
        <span
          className="w-8 shrink-0 text-right text-xs text-label"
          title={open ? `${open} open pull requests` : undefined}
        >
          {open || ""}
        </span>
      </Link>
    </li>
  )
}
