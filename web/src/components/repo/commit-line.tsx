import { Link } from "@tanstack/react-router"

import type { Commit } from "@/lib/api"
import { ago, fullDate, short } from "@/lib/format"
import { href } from "@/lib/paths"
import { Avatar } from "./avatar"

/** The last commit above a directory or file. */
export function CommitLine({
  repo,
  commit,
}: {
  repo: string
  commit: Commit | null
}) {
  if (!commit) return null
  return (
    <div className="flex items-center gap-3 rounded-t-lg border-b bg-muted/40 px-4 py-2.5 text-sm">
      <Avatar name={commit.author.name} />
      <span className="font-medium">{commit.author.name}</span>
      <Link
        to="/$"
        params={{ _splat: href(repo, "commit", commit.oid) }}
        className="min-w-0 truncate text-muted-foreground hover:text-foreground hover:underline"
      >
        {commit.subject}
      </Link>
      <span className="ml-auto flex shrink-0 items-center gap-3 text-xs text-muted-foreground">
        <Link
          to="/$"
          params={{ _splat: href(repo, "commit", commit.oid) }}
          className="font-mono hover:underline"
        >
          {short(commit.oid)}
        </Link>
        <span title={fullDate(commit.committer.time)}>
          {ago(commit.committer.time)}
        </span>
      </span>
    </div>
  )
}
