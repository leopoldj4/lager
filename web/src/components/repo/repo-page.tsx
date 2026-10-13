import { Link } from "@tanstack/react-router"
import { BookMarked, GitCommitHorizontal, GitBranch, Tag } from "lucide-react"

import { useRefs, useWatch } from "@/lib/api"
import { href, splitRev, type Location, type View } from "@/lib/paths"
import { cn } from "@/lib/utils"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import { CloneButton } from "./clone"
import { TreeView } from "./tree"
import { BlobView } from "./blob"
import { CommitList } from "./commits"
import { CommitView } from "./commit"
import { RefList } from "./refs"
import { EmptyRepo } from "./empty"

export function RepoPage({ loc }: { loc: Location }) {
  useWatch(loc.repo)
  const refs = useRefs(loc.repo)
  const names = refs.data && [
    ...refs.data.branches.map((b) => b.name),
    ...refs.data.tags.map((t) => t.name),
  ]
  const split = splitRev(loc.rest, names)
  const rev = split.rev || refs.data?.head || ""
  const empty = refs.data && refs.data.branches.length === 0

  return (
    <div className="grid grid-cols-1 gap-6">
      <div className="flex flex-wrap items-center gap-3">
        <h1 className="flex min-w-0 items-center gap-2 text-xl break-all">
          <BookMarked className="size-5 text-muted-foreground" />
          <RepoName name={loc.repo} />
        </h1>
        <div className="ml-auto">
          <CloneButton repo={loc.repo} />
        </div>
      </div>
      <Tabs
        loc={loc}
        rev={loc.view === "commit" ? "" : split.rev}
        counts={
          refs.data && {
            branches: refs.data.branches.length,
            tags: refs.data.tags.length,
          }
        }
      />
      {refs.error ? (
        <QueryError error={refs.error} />
      ) : !refs.data ? (
        <div className="grid grid-cols-1 gap-2">
          <Skeleton className="h-10" />
          <Skeleton className="h-64" />
        </div>
      ) : loc.view === "branches" ? (
        <RefList repo={loc.repo} kind="branch" refs={refs.data} />
      ) : loc.view === "tags" ? (
        <RefList repo={loc.repo} kind="tag" refs={refs.data} />
      ) : empty ? (
        <EmptyRepo repo={loc.repo} />
      ) : loc.view === "commit" ? (
        <CommitView repo={loc.repo} rev={loc.rest.join("/")} />
      ) : loc.view === "commits" ? (
        <CommitList
          repo={loc.repo}
          rev={rev}
          path={split.path}
          refs={refs.data}
        />
      ) : loc.view === "blob" ? (
        <BlobView
          repo={loc.repo}
          rev={rev}
          path={split.path}
          refs={refs.data}
        />
      ) : (
        <TreeView
          repo={loc.repo}
          rev={rev}
          path={split.path}
          refs={refs.data}
        />
      )}
    </div>
  )
}

function RepoName({ name }: { name: string }) {
  const parts = name.split("/")
  return (
    <Link to="/$" params={{ _splat: href(name) }} className="hover:underline">
      {parts.slice(0, -1).map((p) => (
        <span key={p} className="text-muted-foreground">
          {p} /{" "}
        </span>
      ))}
      <span className="font-semibold">{parts[parts.length - 1]}</span>
    </Link>
  )
}

function Tabs({
  loc,
  rev,
  counts,
}: {
  loc: Location
  rev: string
  counts?: { branches: number; tags: number }
}) {
  const tabs: {
    view: View
    label: string
    icon: typeof GitBranch
    count?: number
  }[] = [
    { view: "tree", label: "Code", icon: BookMarked },
    { view: "commits", label: "Commits", icon: GitCommitHorizontal },
    {
      view: "branches",
      label: "Branches",
      icon: GitBranch,
      count: counts?.branches,
    },
    { view: "tags", label: "Tags", icon: Tag, count: counts?.tags },
  ]
  const active =
    loc.view === "blob" ? "tree" : loc.view === "commit" ? "commits" : loc.view
  return (
    <nav className="-mt-2 flex [scrollbar-width:none] gap-1 overflow-x-auto border-b">
      {tabs.map((t) => (
        <Link
          key={t.view}
          to="/$"
          params={{
            _splat:
              t.view === "tree" && !rev
                ? href(loc.repo)
                : href(
                    loc.repo,
                    t.view,
                    t.view === "tree" || t.view === "commits" ? rev : undefined
                  ),
          }}
          className={cn(
            "-mb-px flex shrink-0 items-center gap-2 border-b-2 px-2.5 py-2 text-sm whitespace-nowrap sm:px-3",
            active === t.view
              ? "border-foreground font-medium"
              : "border-transparent text-muted-foreground hover:text-foreground"
          )}
        >
          <t.icon className="hidden size-4 sm:block" />
          {t.label}
          {t.count !== undefined && (
            <span className="rounded-full bg-muted px-1.5 text-xs">
              {t.count}
            </span>
          )}
        </Link>
      ))}
    </nav>
  )
}
