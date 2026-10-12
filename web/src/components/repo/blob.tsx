import { Link } from "@tanstack/react-router"
import { History } from "lucide-react"

import { useBlob, type Refs } from "@/lib/api"
import { bytes } from "@/lib/format"
import { href } from "@/lib/paths"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import { CommitLine } from "./commit-line"
import { PathCrumbs } from "./path-crumbs"
import { RevPicker } from "./rev-picker"
import { CopyButton } from "./clone"

export function BlobView({
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
  const blob = useBlob(repo, rev, path)
  const lines = blob.data?.text?.replace(/\n$/, "").split("\n")
  return (
    <div className="grid gap-4">
      <div className="flex flex-wrap items-center gap-3">
        <RevPicker repo={repo} view="blob" rev={rev} path={path} refs={refs} />
        <PathCrumbs repo={repo} rev={rev} path={path} />
        <Button asChild variant="ghost" size="sm" className="ml-auto">
          <Link to="/$" params={{ _splat: href(repo, "commits", rev, path) }}>
            <History />
            History
          </Link>
        </Button>
      </div>
      {blob.error ? (
        <QueryError error={blob.error} />
      ) : !blob.data ? (
        <Skeleton className="h-96" />
      ) : (
        <div className="overflow-hidden rounded-lg border">
          <CommitLine repo={repo} commit={blob.data.last_commit} />
          <div className="flex items-center gap-3 border-b px-4 py-2 text-xs text-muted-foreground">
            {lines && <span>{lines.length} lines</span>}
            <span>{bytes(blob.data.size)}</span>
            {blob.data.text !== null && (
              <span className="ml-auto">
                <CopyButton text={blob.data.text} />
              </span>
            )}
          </div>
          {lines ? (
            <div className="overflow-x-auto">
              <table className="w-full font-mono text-[13px] leading-5">
                <tbody>
                  {lines.map((l, i) => (
                    <tr
                      key={i}
                      id={`L${i + 1}`}
                      className="target:bg-amber-500/15"
                    >
                      <td className="w-px pr-4 pl-4 text-right align-top text-muted-foreground select-none">
                        <a href={`#L${i + 1}`}>{i + 1}</a>
                      </td>
                      <td className="pr-4 whitespace-pre">{l || " "}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : (
            <p className="p-10 text-center text-sm text-muted-foreground">
              {blob.data.too_large
                ? "This file is too large to show."
                : "This file is binary and is not shown."}
            </p>
          )}
        </div>
      )}
    </div>
  )
}
