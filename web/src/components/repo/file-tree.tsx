import { useEffect, useRef, useState } from "react"
import { Link } from "@tanstack/react-router"
import {
  ChevronRight,
  File,
  FileSymlink,
  Folder,
  FolderGit2,
  FolderOpen,
} from "lucide-react"

import { useTree, type TreeEntry } from "@/lib/api"
import { href } from "@/lib/paths"
import { cn } from "@/lib/utils"
import { Skeleton } from "@/components/ui/skeleton"

/**
 * The files of a revision as a tree. Each directory loads when it opens;
 * the directories above the current path open by themselves.
 */
export function FileTree({
  repo,
  rev,
  path,
  onNavigate,
}: {
  repo: string
  rev: string
  /** The directory or file the page shows. */
  path: string
  /** Called when a link is followed, to close a sheet around the tree. */
  onNavigate?: () => void
}) {
  const [open, setOpen] = useState(() => new Set(ancestors(path)))
  useEffect(() => {
    setOpen((o) => new Set([...o, ...ancestors(path)]))
  }, [path])
  const toggle = (dir: string) =>
    setOpen((o) => {
      const n = new Set(o)
      if (!n.delete(dir)) n.add(dir)
      return n
    })
  return (
    <nav aria-label="Files" className="py-2 text-sm">
      <Directory
        tree={{ repo, rev, current: path, open, toggle, onNavigate }}
        dir=""
        depth={0}
      />
    </nav>
  )
}

interface TreeState {
  repo: string
  rev: string
  current: string
  open: Set<string>
  toggle: (dir: string) => void
  onNavigate?: () => void
}

/** "a", "a/b", and "a/b/c" for "a/b/c". */
function ancestors(path: string) {
  const parts = path.split("/").filter(Boolean)
  return parts.map((_, i) => parts.slice(0, i + 1).join("/"))
}

function join(dir: string, name: string) {
  return dir ? `${dir}/${name}` : name
}

export const indent = (depth: number) => ({
  paddingLeft: `${0.5 + depth * 0.75}rem`,
})

function Directory({
  tree,
  dir,
  depth,
}: {
  tree: TreeState
  dir: string
  depth: number
}) {
  const q = useTree(tree.repo, tree.rev, dir)
  if (q.error) {
    return (
      <p className="py-1 text-xs text-destructive" style={indent(depth)}>
        {q.error.message}
      </p>
    )
  }
  if (!q.data) {
    return (
      <div className="grid gap-1 py-1 pr-2" style={indent(depth)}>
        <Skeleton className="h-5" />
        <Skeleton className="h-5 w-2/3" />
      </div>
    )
  }
  // Directories first, as in a file manager; Git sorts by name only.
  const entries = [...q.data.entries].sort(
    (a, b) => Number(b.kind === "tree") - Number(a.kind === "tree")
  )
  return (
    <ul>
      {entries.map((e) => (
        <Node key={e.name} tree={tree} dir={dir} entry={e} depth={depth} />
      ))}
      {q.data.truncated && (
        <li
          className="py-1 text-xs text-muted-foreground"
          style={indent(depth)}
        >
          Only the first {q.data.entries.length} entries are shown.
        </li>
      )}
    </ul>
  )
}

export const row =
  "flex h-7 min-w-0 items-center gap-1.5 rounded-md pr-2 hover:bg-muted"

function Node({
  tree,
  dir,
  entry,
  depth,
}: {
  tree: TreeState
  dir: string
  entry: TreeEntry
  depth: number
}) {
  const path = join(dir, entry.name)
  const active = path === tree.current
  const ref = useRef<HTMLLIElement>(null)
  useEffect(() => {
    if (active) ref.current?.scrollIntoView({ block: "nearest" })
  }, [active])

  if (entry.kind === "submodule") {
    return (
      <li
        className={cn(row, "text-muted-foreground hover:bg-transparent")}
        style={indent(depth)}
        title={`Submodule at ${entry.oid.slice(0, 8)}`}
      >
        <span className="size-4 shrink-0" />
        <FolderGit2 className="size-4 shrink-0" />
        <span className="truncate">{entry.name}</span>
      </li>
    )
  }

  const isDir = entry.kind === "tree"
  const open = isDir && tree.open.has(path)
  const Icon = isDir
    ? open
      ? FolderOpen
      : Folder
    : entry.kind === "symlink"
      ? FileSymlink
      : File
  return (
    <li ref={ref}>
      <div
        className={cn(row, active && "bg-muted font-medium")}
        style={indent(depth)}
      >
        {isDir ? (
          <button
            type="button"
            onClick={() => tree.toggle(path)}
            className="flex size-4 shrink-0 items-center justify-center rounded-sm text-muted-foreground hover:text-foreground"
            aria-label={open ? `Close ${entry.name}` : `Open ${entry.name}`}
            aria-expanded={open}
          >
            <ChevronRight
              className={cn(
                "size-3.5 transition-transform",
                open && "rotate-90"
              )}
            />
          </button>
        ) : (
          <span className="size-4 shrink-0" />
        )}
        <Link
          to="/$"
          params={{
            _splat: href(tree.repo, isDir ? "tree" : "blob", tree.rev, path),
          }}
          onClick={() => {
            // A second click on the open directory closes it.
            if (isDir && active) tree.toggle(path)
            tree.onNavigate?.()
          }}
          className="flex min-w-0 flex-1 items-center gap-1.5"
          aria-current={active ? "page" : undefined}
        >
          <Icon
            className={cn(
              "size-4 shrink-0",
              isDir ? "fill-sky-500/20 text-sky-600" : "text-muted-foreground"
            )}
          />
          <span className="truncate">{entry.name}</span>
        </Link>
      </div>
      {open && <Directory tree={tree} dir={path} depth={depth + 1} />}
    </li>
  )
}
