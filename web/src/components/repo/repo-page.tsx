import { Link } from "@tanstack/react-router"
import { Plus } from "lucide-react"

import { useLive } from "@/lib/query"
import { useRefs, useWatch } from "@/lib/repos"
import type { Refs } from "@/lib/wire"
import { short } from "@/lib/format"
import { href, splitRev, type Location } from "@/lib/paths"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import {
  BranchChip,
  Crumbs,
  PageBody,
  PageHeader,
  type Crumb,
} from "@/components/frame"
import { CloneButton } from "./clone"
import { CodeView } from "./code"
import { CommitList } from "./commits"
import { CommitView } from "./commit"
import { BranchList } from "./refs"
import { EmptyRepo } from "./empty"
import { newPullHref, PullList } from "./pulls"
import { PullView } from "./pull"
import { ComparePage } from "./compare"
import { Overview } from "./overview"
import { RepoSettings } from "./settings"
import { RevPicker } from "./rev-picker"

/**
 * A page of a repository. Its sections are in the sidebar; the header
 * names the page and holds at most two actions.
 */
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
  // Only the owner changes the settings (the server checks it too).
  const owner = useLive()?.user === loc.repo.split("/")[0]
  const code = loc.view === "tree" || loc.view === "blob"

  const page = refs.error ? (
    <PageBody>
      <QueryError error={refs.error} />
    </PageBody>
  ) : !refs.data ? (
    <PageBody className="grid grid-cols-1 gap-2">
      <Skeleton className="h-24" />
      <Skeleton className="h-64" />
    </PageBody>
  ) : loc.view === "settings" ? (
    <PageBody>
      {owner ? (
        <RepoSettings repo={loc.repo} refs={refs.data} />
      ) : (
        <p className="text-sm text-muted-foreground">
          Only {loc.repo.split("/")[0]} changes the settings of this repository.
        </p>
      )}
    </PageBody>
  ) : loc.view === "branches" ? (
    <PageBody>
      <BranchList repo={loc.repo} refs={refs.data} />
    </PageBody>
  ) : loc.view === "pulls" ? (
    <PageBody>
      <PullList
        repo={loc.repo}
        closed={loc.rest[0] === "closed"}
        refs={refs.data}
      />
    </PageBody>
  ) : loc.view === "pull" ? (
    <PullView repo={loc.repo} rest={loc.rest} />
  ) : loc.view === "compare" ? (
    <PageBody>
      <ComparePage repo={loc.repo} rest={loc.rest} refs={refs.data} />
    </PageBody>
  ) : empty ? (
    <PageBody>
      <EmptyRepo repo={loc.repo} mirror={refs.data.mirror} />
    </PageBody>
  ) : loc.view === "overview" ? (
    <Overview repo={loc.repo} refs={refs.data} />
  ) : loc.view === "commit" ? (
    <PageBody>
      <CommitView repo={loc.repo} rev={loc.rest.join("/")} />
    </PageBody>
  ) : loc.view === "commits" ? (
    <PageBody>
      <CommitList repo={loc.repo} rev={rev} path={split.path} />
    </PageBody>
  ) : (
    <CodeView
      repo={loc.repo}
      rev={rev}
      path={split.path}
      view={loc.view === "blob" ? "blob" : "tree"}
    />
  )

  return (
    <>
      <PageHeader
        actions={
          refs.data && <HeaderActions loc={loc} refs={refs.data} code={code} />
        }
      >
        <Crumbs items={crumbs(loc, split.path, rev)} />
        {refs.data &&
          !empty &&
          (code || loc.view === "commits" ? (
            <RevPicker
              repo={loc.repo}
              view={loc.view}
              rev={rev}
              path={split.path}
              refs={refs.data}
            />
          ) : (
            loc.view === "overview" && (
              <BranchChip name={refs.data.head} className="hidden sm:flex" />
            )
          ))}
      </PageHeader>
      {page}
    </>
  )
}

/** owner / repo, then the page, then a path in the code. */
function crumbs(loc: Location, path: string, rev: string): Crumb[] {
  const [owner, ...rest] = loc.repo.split("/")
  const out: Crumb[] = [
    { label: owner, to: owner },
    { label: rest.join("/"), to: href(loc.repo) },
  ]
  const page = (label: string, to?: string) => out.push({ label, to })
  switch (loc.view) {
    case "tree":
    case "blob": {
      const parts = path.split("/").filter(Boolean)
      if (parts.length > 0) out[1].to = href(loc.repo, "tree", rev)
      parts.forEach((p, i) =>
        out.push({
          label: p,
          to: href(loc.repo, "tree", rev, parts.slice(0, i + 1).join("/")),
        })
      )
      break
    }
    case "commits":
      page("commits", href(loc.repo, "commits", rev))
      if (path) page(path)
      break
    case "commit":
      page("commits", href(loc.repo, "commits"))
      out.push({ label: short(loc.rest.join("/")), mono: true })
      break
    case "branches":
      page("branches")
      break
    case "pulls":
      page("pull requests", href(loc.repo, "pulls"))
      if (loc.rest[0] === "closed") page("closed")
      break
    case "pull":
      page("pull requests", href(loc.repo, "pulls"))
      page(`#${loc.rest[0] ?? ""}`)
      break
    case "compare":
      page("compare")
      break
    case "settings":
      page("settings")
      break
  }
  return out
}

function HeaderActions({
  loc,
  refs,
  code,
}: {
  loc: Location
  refs: Refs
  code: boolean
}) {
  const user = useLive()?.user
  const newPull = user && refs.branches.length > 1 && (
    <Button asChild size="sm">
      <Link to="/$" params={{ _splat: newPullHref(loc.repo, refs) }}>
        <Plus />
        <span className="hidden sm:inline">New pull request</span>
      </Link>
    </Button>
  )
  if (loc.view === "overview" || loc.view === "pulls") return newPull
  if (code)
    return (
      <>
        <CloneButton repo={loc.repo} variant="outline" />
        {newPull}
      </>
    )
  return null
}
