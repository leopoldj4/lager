import { Link } from "@tanstack/react-router"
import { FileDown, History } from "lucide-react"

import type { Refs } from "@/lib/api"
import { href, rawHref } from "@/lib/paths"
import { Button } from "@/components/ui/button"
import { BlobView } from "./blob"
import { FileTree } from "./file-tree"
import { PathCrumbs } from "./path-crumbs"
import { RevPicker } from "./rev-picker"
import { SidePanelLayout } from "./side-panel"
import { TreeView } from "./tree"

/**
 * The code of a revision: the file tree beside the directory or file the
 * address names. On phones the tree opens in a sheet.
 */
export function CodeView({
  repo,
  rev,
  path,
  refs,
  view,
}: {
  repo: string
  rev: string
  path: string
  refs: Refs
  view: "tree" | "blob"
}) {
  const preview =
    view === "blob" ? (
      <BlobView repo={repo} rev={rev} path={path} />
    ) : (
      <TreeView repo={repo} rev={rev} path={path} />
    )
  const tree = (onNavigate?: () => void) => (
    <FileTree repo={repo} rev={rev} path={path} onNavigate={onNavigate} />
  )

  return (
    <SidePanelLayout
      id="worktree-code"
      title="Files"
      side={tree}
      toolbar={
        <>
          <RevPicker
            repo={repo}
            view={view}
            rev={rev}
            path={path}
            refs={refs}
          />
          <PathCrumbs repo={repo} rev={rev} path={path} />
          <div className="ml-auto flex gap-1">
            {view === "blob" && (
              <Button asChild variant="ghost" size="sm">
                <a href={rawHref(repo, rev, path)}>
                  <FileDown />
                  Raw
                </a>
              </Button>
            )}
            <Button asChild variant="ghost" size="sm">
              <Link
                to="/$"
                params={{ _splat: href(repo, "commits", rev, path) }}
              >
                <History />
                History
              </Link>
            </Button>
          </div>
        </>
      }
    >
      {preview}
    </SidePanelLayout>
  )
}
