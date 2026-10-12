import { Link } from "@tanstack/react-router"
import { useMemo } from "react"

import { useCommit } from "@/lib/api"
import { fullDate, short } from "@/lib/format"
import { href } from "@/lib/paths"
import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import { Avatar } from "./avatar"
import { CopyButton } from "./clone"
import { parseDiff, type DiffFile } from "./diff"

export function CommitView({ repo, rev }: { repo: string; rev: string }) {
  const q = useCommit(repo, rev)
  const files = useMemo(() => (q.data ? parseDiff(q.data.diff) : []), [q.data])
  if (q.error) return <QueryError error={q.error} />
  if (!q.data) return <Skeleton className="h-96" />
  const { commit, truncated } = q.data
  const body = commit.message.split("\n").slice(1).join("\n").trim()
  const adds = files.reduce((n, f) => n + f.adds, 0)
  const dels = files.reduce((n, f) => n + f.dels, 0)

  return (
    <div className="grid gap-6">
      <div className="rounded-lg border">
        <div className="p-4">
          <h2 className="text-lg font-semibold">{commit.subject}</h2>
          {body && (
            <pre className="mt-3 font-mono text-sm whitespace-pre-wrap text-muted-foreground">
              {body}
            </pre>
          )}
        </div>
        <div className="flex flex-wrap items-center gap-3 border-t bg-muted/40 px-4 py-2.5 text-sm">
          <Avatar name={commit.author.name} />
          <span>
            <span className="font-medium">{commit.author.name}</span>{" "}
            <span className="text-muted-foreground">
              authored {fullDate(commit.author.time)}
            </span>
          </span>
          {commit.committer.name !== commit.author.name && (
            <span className="text-muted-foreground">
              committed by{" "}
              <span className="text-foreground">{commit.committer.name}</span>
            </span>
          )}
          <span className="ml-auto flex items-center gap-2 font-mono text-xs text-muted-foreground">
            {commit.parents.length > 0 && (
              <span>
                {commit.parents.length > 1 ? "parents" : "parent"}{" "}
                {commit.parents.map((p) => (
                  <Link
                    key={p}
                    to="/$"
                    params={{ _splat: href(repo, "commit", p) }}
                    className="mr-1 hover:underline"
                  >
                    {short(p)}
                  </Link>
                ))}
              </span>
            )}
            <span className="text-foreground">{short(commit.oid)}</span>
            <CopyButton text={commit.oid} />
            <Button asChild size="sm" variant="outline" className="font-sans">
              <Link to="/$" params={{ _splat: href(repo, "tree", commit.oid) }}>
                Browse files
              </Link>
            </Button>
          </span>
        </div>
      </div>
      <p className="text-sm text-muted-foreground">
        {files.length} {files.length === 1 ? "file" : "files"} changed,{" "}
        <span className="text-emerald-600">+{adds}</span>{" "}
        <span className="text-red-600">−{dels}</span>
        {commit.parents.length > 1 && " (against the first parent)"}
      </p>
      {files.map((f, i) => (
        <FileDiff key={i} file={f} />
      ))}
      {truncated && (
        <p className="rounded-lg border border-dashed p-6 text-center text-sm text-muted-foreground">
          The diff is too large to show completely.
        </p>
      )}
    </div>
  )
}

function FileDiff({ file }: { file: DiffFile }) {
  return (
    <div className="overflow-hidden rounded-lg border">
      <div className="flex items-center gap-3 border-b bg-muted/40 px-4 py-2 font-mono text-sm">
        <span className="min-w-0 truncate">
          {file.oldPath && file.oldPath !== file.path
            ? `${file.oldPath} → ${file.path}`
            : file.path}
        </span>
        {file.status && (
          <span className="rounded bg-muted px-1.5 text-xs text-muted-foreground">
            {file.status}
          </span>
        )}
        <span className="ml-auto text-xs">
          <span className="text-emerald-600">+{file.adds}</span>{" "}
          <span className="text-red-600">−{file.dels}</span>
        </span>
      </div>
      {file.binary ? (
        <p className="p-4 text-sm text-muted-foreground">
          Binary file changed.
        </p>
      ) : file.lines.length === 0 ? (
        <p className="p-4 text-sm text-muted-foreground">No content changes.</p>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full font-mono text-[13px] leading-5">
            <tbody>
              {file.lines.map((l, i) => (
                <tr
                  key={i}
                  className={cn(
                    l.kind === "add" && "bg-emerald-500/10",
                    l.kind === "del" && "bg-red-500/10",
                    l.kind === "hunk" && "bg-sky-500/10 text-muted-foreground"
                  )}
                >
                  <td className="w-px px-2 text-right text-muted-foreground select-none">
                    {l.old ?? ""}
                  </td>
                  <td className="w-px px-2 text-right text-muted-foreground select-none">
                    {l.new ?? ""}
                  </td>
                  <td className="pr-4 whitespace-pre">
                    {l.kind === "hunk" ? l.text : l.text || " "}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  )
}
