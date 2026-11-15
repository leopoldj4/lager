import { Link } from "@tanstack/react-router"
import { File, FileSymlink, Folder, FolderGit2 } from "lucide-react"

import { useBlob, useTree } from "@/lib/repos"
import type { TreeEntry } from "@/lib/wire"
import { bytes } from "@/lib/format"
import { href } from "@/lib/paths"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import { isMarkdown, MarkdownView } from "./markdown"

const icons = {
  tree: Folder,
  file: File,
  symlink: FileSymlink,
  submodule: FolderGit2,
}

/** A directory: its entries and its README. */
export function TreeView({
  repo,
  rev,
  path,
}: {
  repo: string
  rev: string
  path: string
}) {
  const tree = useTree(repo, rev, path)
  const readme = tree.data && findReadme(tree.data.entries)
  return (
    <div>
      {tree.error ? (
        <div className="p-4 md:p-6">
          <QueryError error={tree.error} />
        </div>
      ) : !tree.data ? (
        <Skeleton className="m-4 h-72 md:m-6" />
      ) : (
        <div>
          <ul className="text-[13px] [&>li]:border-b [&>li]:border-hairline">
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
                  className="flex h-9 items-center px-3 text-muted-foreground hover:bg-muted/50 md:px-4"
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
            <p className="border-b border-hairline px-3 py-2 text-xs text-muted-foreground md:px-4">
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
    <span className="flex min-w-0 items-center gap-2.5">
      <Icon
        className={
          entry.kind === "tree"
            ? "size-4 shrink-0 fill-muted text-muted-foreground"
            : "size-4 shrink-0 text-label"
        }
      />
      <span
        className={
          entry.kind === "tree"
            ? "truncate font-medium text-ink"
            : "truncate text-body"
        }
      >
        {entry.name}
      </span>
    </span>
  )
  if (entry.kind === "submodule") {
    return (
      <li className="flex h-9 items-center px-3 md:px-4">
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
        className="flex h-9 items-center px-3 hover:bg-muted/50 md:px-4"
      >
        {name}
        {entry.size !== null && (
          <span className="ml-auto shrink-0 pl-3 font-mono text-[11px] text-label">
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
    <div className="mt-8 border-t border-hairline">
      <h2 className="border-b border-hairline px-3 py-2 label-caps md:px-4">
        {path.split("/").pop()}
      </h2>
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
