import { Link } from "@tanstack/react-router"

import { useCommit } from "@/lib/api"
import { fullDate, short } from "@/lib/format"
import { href } from "@/lib/paths"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import { Avatar } from "./avatar"
import { ChangedFiles } from "./changes"
import { CopyButton } from "./clone"

export function CommitView({ repo, rev }: { repo: string; rev: string }) {
  const q = useCommit(repo, rev)
  if (q.error) return <QueryError error={q.error} />
  if (!q.data) return <Skeleton className="h-96" />
  const { commit, diff, truncated } = q.data
  const body = commit.message.split("\n").slice(1).join("\n").trim()

  return (
    <div className="grid grid-cols-1 gap-6">
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
      {/* Merges show their changes against the first parent. */}
      <ChangedFiles
        repo={repo}
        diff={diff}
        truncated={truncated}
        oldRev={commit.parents[0]}
        newRev={commit.oid}
        note={
          commit.parents.length > 1 ? " (against the first parent)" : undefined
        }
      />
    </div>
  )
}
