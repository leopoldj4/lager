import { Link } from "@tanstack/react-router"

import type { Commit } from "@/lib/wire"
import { fullDate, short, since } from "@/lib/format"
import { href } from "@/lib/paths"
import { Avatar } from "./avatar"
import { AuthorName } from "./author-name"

/** The last commit above a directory or file, on one line. */
export function CommitLine({
  repo,
  commit,
}: {
  repo: string
  commit: Commit | null | undefined
}) {
  if (!commit) return <span className="flex-1" />
  return (
    <div className="flex min-w-0 flex-1 items-center gap-2.5 text-[13px]">
      <Avatar name={commit.author.name} className="size-5 text-[9px]" />
      <AuthorName
        author={commit.author}
        className="hidden shrink-0 font-medium text-ink sm:inline"
      />
      <Link
        to="/$"
        params={{ _splat: href(repo, "commit", commit.oid) }}
        className="min-w-0 truncate text-body hover:text-ink"
      >
        {commit.subject}
      </Link>
      <span className="ml-auto flex shrink-0 items-center gap-1.5 text-[11px] text-label">
        <Link
          to="/$"
          params={{ _splat: href(repo, "commit", commit.oid) }}
          className="hidden font-mono hover:text-ink sm:inline"
        >
          {short(commit.oid)}
        </Link>
        <span className="hidden sm:inline">·</span>
        <span title={fullDate(commit.committer.time)}>
          {since(commit.committer.time)}
        </span>
      </span>
    </div>
  )
}
