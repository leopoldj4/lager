import { Link } from "@tanstack/react-router"
import { GitBranch } from "lucide-react"

import type { Refs } from "@/lib/wire"
import { fullDate, short, since } from "@/lib/format"
import { href } from "@/lib/paths"
import { EmptyRow, PageTitle, SectionHead, rowList } from "@/components/frame"
import { Badge } from "@/components/ui/badge"

export function BranchList({ repo, refs }: { repo: string; refs: Refs }) {
  const list = [...refs.branches].sort((a, b) => b.time - a.time)
  return (
    <div className="grid grid-cols-1 gap-8">
      <PageTitle>Branches</PageTitle>
      <section>
        <SectionHead title="By last push" count={list.length} />
        {list.length === 0 ? (
          <EmptyRow>No branches yet.</EmptyRow>
        ) : (
          <ul className={rowList}>
            {list.map((r) => (
              <li
                key={r.name}
                className="flex h-11 items-center gap-3 px-1 text-[13px]"
              >
                <GitBranch className="size-3.5 shrink-0 text-label" />
                <Link
                  to="/$"
                  params={{ _splat: href(repo, "tree", r.name) }}
                  className="shrink-0 font-mono text-xs font-medium text-ink hover:underline"
                >
                  {r.name}
                </Link>
                {r.name === refs.head && (
                  <Badge variant="outline">default</Badge>
                )}
                <span className="min-w-0 flex-1 truncate text-body">
                  {r.subject}
                </span>
                <Link
                  to="/$"
                  params={{ _splat: href(repo, "commit", r.oid) }}
                  className="hidden w-16 shrink-0 font-mono text-[11px] text-label hover:text-ink sm:inline"
                >
                  {short(r.oid).slice(0, 7)}
                </Link>
                <span
                  className="w-7 shrink-0 text-right text-xs text-label"
                  title={fullDate(r.time)}
                >
                  {since(r.time)}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  )
}
