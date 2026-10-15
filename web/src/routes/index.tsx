import { Link, createFileRoute } from "@tanstack/react-router"
import { BookLock, BookOpen, Search } from "lucide-react"
import { useState } from "react"

import { useLive, useRepos } from "@/lib/api"
import { href } from "@/lib/paths"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import { CreateRepoDialog } from "@/components/create-repo-dialog"

export const Route = createFileRoute("/")({ component: Repositories })

function Repositories() {
  const repos = useRepos()
  const state = useLive()
  const [filter, setFilter] = useState("")
  const shown = repos.data?.repos.filter((r) =>
    r.name.toLowerCase().includes(filter.toLowerCase())
  )

  return (
    <div className="mx-auto grid w-full max-w-6xl grid-cols-1 gap-6">
      <div className="flex flex-wrap items-center gap-4">
        <h1 className="text-xl font-semibold">Repositories</h1>
        <div className="ml-auto">
          <CreateRepoDialog />
        </div>
        <div className="relative w-full sm:w-72">
          <Search className="absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            placeholder="Find a repository"
            className="pl-8"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
        </div>
      </div>
      {repos.error ? (
        <QueryError error={repos.error} />
      ) : !shown ? (
        <div className="grid grid-cols-1 gap-2">
          {[0, 1, 2].map((i) => (
            <Skeleton key={i} className="h-14" />
          ))}
        </div>
      ) : shown.length === 0 ? (
        <p className="rounded-lg border border-dashed p-10 text-center text-sm text-muted-foreground">
          {filter
            ? "No repository matches."
            : state?.user
              ? "No repositories yet. Create one or push to a new name."
              : "No public repositories. Sign in to see yours."}
        </p>
      ) : (
        <ul className="divide-y rounded-lg border">
          {shown.map((r) => (
            <li key={r.name}>
              <Link
                to="/$"
                params={{ _splat: href(r.name) }}
                className="flex items-center gap-3 px-4 py-3 hover:bg-muted/50"
              >
                {r.anonymous_read ? (
                  <BookOpen className="size-4 text-muted-foreground" />
                ) : (
                  <BookLock className="size-4 text-muted-foreground" />
                )}
                <span className="font-medium">{r.name}</span>
                {r.anonymous_read && <Badge variant="outline">public</Badge>}
                <span className="ml-auto font-mono text-xs text-muted-foreground">
                  {r.default_branch}
                </span>
              </Link>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
