import { Link } from "@tanstack/react-router"
import { GitBranch, Tag } from "lucide-react"

import type { Refs } from "@/lib/api"
import { ago, fullDate, short } from "@/lib/format"
import { href } from "@/lib/paths"
import { Badge } from "@/components/ui/badge"

export function RefList({
  repo,
  kind,
  refs,
}: {
  repo: string
  kind: "branch" | "tag"
  refs: Refs
}) {
  const list = [...(kind === "branch" ? refs.branches : refs.tags)].sort(
    (a, b) => b.time - a.time
  )
  const Icon = kind === "branch" ? GitBranch : Tag
  if (list.length === 0) {
    return (
      <p className="rounded-lg border border-dashed p-10 text-center text-sm text-muted-foreground">
        {kind === "branch" ? "No branches yet." : "No tags yet."}
      </p>
    )
  }
  return (
    <ul className="divide-y rounded-lg border">
      {list.map((r) => (
        <li key={r.name} className="flex items-center gap-3 px-4 py-3 text-sm">
          <Icon className="size-4 text-muted-foreground" />
          <Link
            to="/$"
            params={{ _splat: href(repo, "tree", r.name) }}
            className="font-mono font-medium hover:underline"
          >
            {r.name}
          </Link>
          {kind === "branch" && r.name === refs.head && (
            <Badge variant="secondary">default</Badge>
          )}
          <span className="min-w-0 flex-1 truncate text-muted-foreground">
            {r.subject}
          </span>
          <span
            className="shrink-0 text-xs text-muted-foreground"
            title={fullDate(r.time)}
          >
            {ago(r.time)}
          </span>
          <Link
            to="/$"
            params={{ _splat: href(repo, "commit", r.oid) }}
            className="shrink-0 font-mono text-xs text-muted-foreground hover:underline"
          >
            {short(r.oid)}
          </Link>
        </li>
      ))}
    </ul>
  )
}
