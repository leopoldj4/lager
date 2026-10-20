import { Link } from "@tanstack/react-router"
import { GitBranch } from "lucide-react"

import type { Refs } from "@/lib/api"
import { ago, fullDate, short } from "@/lib/format"
import { href } from "@/lib/paths"
import { Badge } from "@/components/ui/badge"

export function BranchList({ repo, refs }: { repo: string; refs: Refs }) {
  const list = [...refs.branches].sort((a, b) => b.time - a.time)
  if (list.length === 0) {
    return (
      <p className="rounded-lg border border-dashed p-10 text-center text-sm text-muted-foreground">
        No branches yet.
      </p>
    )
  }
  return (
    <ul className="divide-y rounded-lg border">
      {list.map((r) => (
        <li key={r.name} className="flex items-center gap-3 px-4 py-3 text-sm">
          <GitBranch className="size-4 text-muted-foreground" />
          <Link
            to="/$"
            params={{ _splat: href(repo, "tree", r.name) }}
            className="font-mono font-medium hover:underline"
          >
            {r.name}
          </Link>
          {r.name === refs.head && <Badge variant="secondary">default</Badge>}
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
