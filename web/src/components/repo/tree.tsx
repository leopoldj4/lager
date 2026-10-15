import { Link } from "@tanstack/react-router"
import { File, FileSymlink, Folder, FolderGit2, History } from "lucide-react"

import { useBlob, useTree, type Refs, type TreeEntry } from "@/lib/api"
import { bytes } from "@/lib/format"
import { href } from "@/lib/paths"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import { CommitLine } from "./commit-line"
import { PathCrumbs } from "./path-crumbs"
import { RevPicker } from "./rev-picker"
import { isMarkdown, MarkdownView } from "./markdown"

const icons = {
  tree: Folder,
  file: File,
  symlink: FileSymlink,
  submodule: FolderGit2,
}

export function TreeView({
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
  const tree = useTree(repo, rev, path)
  const readme = tree.data && findReadme(tree.data.entries)
  return (
    <div className="grid grid-cols-1 gap-4">
      <div className="flex flex-wrap items-center gap-3">
        <RevPicker repo={repo} view="tree" rev={rev} path={path} refs={refs} />
        <PathCrumbs repo={repo} rev={rev} path={path} />
        <Button asChild variant="ghost" size="sm" className="ml-auto">
          <Link to="/$" params={{ _splat: href(repo, "commits", rev, path) }}>
            <History />
            History
          </Link>
        </Button>
      </div>
      {tree.error ? (
        <QueryError error={tree.error} />
      ) : !tree.data ? (
        <Skeleton className="h-72" />
      ) : (
        <div className="rounded-lg border">
          <CommitLine repo={repo} commit={tree.data.last_commit} />
          <ul className="divide-y text-sm">
            {path && (
              <li>
                <Link
                  to="/$"
                  params={{
                    _splat: href(
                      repo,
                      "tree",
                      rev,
                      path.split("/").slice(0, -1).join("/")
                    ),
                  }}
                  className="block px-4 py-2 text-muted-foreground hover:bg-muted/50"
                >
                  ..
                </Link>
              </li>
            )}
            {tree.data.entries.map((e) => (
              <Entry key={e.name} repo={repo} rev={rev} dir={path} entry={e} />
            ))}
          </ul>
          {tree.data.truncated && (
            <p className="border-t px-4 py-2 text-xs text-muted-foreground">
              Only the first {tree.data.entries.length} entries are shown.
            </p>
          )}
        </div>
      )}
      {readme && (
        <Readme repo={repo} rev={rev} path={join(path, readme.name)} />
      )}
    </div>
  )
}

/** The README among a directory's entries. */
export function findReadme(entries: TreeEntry[]) {
  return entries.find(
    (e) =>
      e.kind === "file" && /^readme(\.(md|markdown|txt|rst))?$/i.test(e.name)
  )
}

function join(dir: string, name: string) {
  return dir ? `${dir}/${name}` : name
}

function Entry({
  repo,
  rev,
  dir,
  entry,
}: {
  repo: string
  rev: string
  dir: string
  entry: TreeEntry
}) {
  const Icon = icons[entry.kind]
  const name = (
    <span className="flex items-center gap-3">
      <Icon
        className={
          entry.kind === "tree"
            ? "size-4 fill-sky-500/20 text-sky-600"
            : "size-4 text-muted-foreground"
        }
      />
      <span className="truncate">{entry.name}</span>
    </span>
  )
  if (entry.kind === "submodule") {
    return (
      <li className="flex items-center px-4 py-2">
        {name}
        <span className="ml-auto font-mono text-xs text-muted-foreground">
          @ {entry.oid.slice(0, 8)}
        </span>
      </li>
    )
  }
  return (
    <li>
      <Link
        to="/$"
        params={{
          _splat: href(
            repo,
            entry.kind === "tree" ? "tree" : "blob",
            rev,
            join(dir, entry.name)
          ),
        }}
        className="flex items-center px-4 py-2 hover:bg-muted/50"
      >
        {name}
        {entry.size !== null && (
          <span className="ml-auto text-xs text-muted-foreground">
            {bytes(entry.size)}
          </span>
        )}
      </Link>
    </li>
  )
}

export function Readme({
  repo,
  rev,
  path,
}: {
  repo: string
  rev: string
  path: string
}) {
  const blob = useBlob(repo, rev, path)
  if (!blob.data?.text) return null
  return (
    <div className="rounded-lg border">
      <div className="border-b px-4 py-2 text-sm font-medium">
        {path.split("/").pop()}
      </div>
      {isMarkdown(path) ? (
        <MarkdownView repo={repo} rev={rev} path={path} text={blob.data.text} />
      ) : (
        <pre className="overflow-x-auto p-6 font-mono text-sm leading-relaxed whitespace-pre-wrap">
          {blob.data.text}
        </pre>
      )}
    </div>
  )
}
