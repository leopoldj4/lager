import { Link } from "@tanstack/react-router"
import { FileDown, History } from "lucide-react"

import { useBlob, useTree } from "@/lib/repos"
import { href, rawHref } from "@/lib/paths"
import { Button } from "@/components/ui/button"
import { BlobView } from "./blob"
import { CommitLine } from "./commit-line"
import { FileTree } from "./file-tree"
import { SidePanelLayout } from "./side-panel"
import { TreeView } from "./tree"

/**
 * The code of a revision: the file tree beside the directory or file the
 * address names, under the commit that last changed it. On phones the tree
 * opens in a sheet.
 */
export function CodeView({
  repo,
  rev,
  path,
  view,
}: {
  repo: string
  rev: string
  path: string
  view: "tree" | "blob"
}) {
  // The same queries as the views below; they share the answers.
  const tree = useTree(repo, rev, view === "tree" ? path : "")
  const blob = useBlob(repo, rev, path, view === "blob")
  const last = view === "blob" ? blob.data?.last_commit : tree.data?.last_commit

  return (
    <SidePanelLayout
      id="lager-code"
      title="Files"
      side={(onNavigate) => (
        <FileTree repo={repo} rev={rev} path={path} onNavigate={onNavigate} />
      )}
      toolbar={
        <>
          <CommitLine repo={repo} commit={last} />
          <div className="flex shrink-0 gap-0.5">
            {view === "blob" && (
              <Button
                asChild
                variant="ghost"
                size="icon-sm"
                className="text-muted-foreground"
              >
                <a href={rawHref(repo, rev, path)} title="Raw" aria-label="Raw">
                  <FileDown />
                </a>
              </Button>
            )}
            <Button
              asChild
              variant="ghost"
              size="icon-sm"
              className="text-muted-foreground"
            >
              <Link
                to="/$"
                params={{ _splat: href(repo, "commits", rev, path) }}
                title="History"
                aria-label="History"
              >
                <History />
              </Link>
            </Button>
          </div>
        </>
      }
    >
      {view === "blob" ? (
        <BlobView repo={repo} rev={rev} path={path} />
      ) : (
        <TreeView repo={repo} rev={rev} path={path} />
      )}
    </SidePanelLayout>
  )
}
