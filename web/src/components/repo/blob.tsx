import type { SelectedLineRange } from "@pierre/diffs"
import { File } from "@pierre/diffs/react"
import { Link } from "@tanstack/react-router"
import { History } from "lucide-react"
import { useMemo, useState, type ReactNode } from "react"

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
import { codeOptions, codeStyle } from "./diff"

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
  return (
    <div className="grid grid-cols-1 gap-4">
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
          {blob.data.text !== null ? (
            <Code
              path={blob.data.path}
              text={blob.data.text}
              header={<FileInfo size={blob.data.size} text={blob.data.text} />}
            />
          ) : (
            <>
              <FileInfo size={blob.data.size} text={null} />
              <p className="p-10 text-center text-sm text-muted-foreground">
                {blob.data.too_large
                  ? "This file is too large to show."
                  : "This file is binary and is not shown."}
              </p>
            </>
          )}
        </div>
      )}
    </div>
  )
}

function FileInfo({ size, text }: { size: number; text: string | null }) {
  return (
    <div className="flex items-center gap-3 border-b px-4 py-2 text-xs text-muted-foreground">
      {text !== null && (
        <span>{text.replace(/\n$/, "").split("\n").length} lines</span>
      )}
      <span>{bytes(size)}</span>
      {text !== null && (
        <span className="ml-auto">
          <CopyButton text={text} />
        </span>
      )}
    </div>
  )
}

// Selected lines live in the URL fragment, as #L12 or #L12-L20.
function readLines(): SelectedLineRange | null {
  const m = /^#L(\d+)(?:-L(\d+))?$/.exec(location.hash)
  return m ? { start: Number(m[1]), end: Number(m[2] ?? m[1]) } : null
}

function writeLines(r: SelectedLineRange | null) {
  const [a, b] = r ? [r.start, r.end].sort((x, y) => x - y) : []
  const hash = !r ? "" : a === b ? `#L${a}` : `#L${a}-L${b}`
  history.replaceState(
    history.state,
    "",
    location.pathname + location.search + hash
  )
}

function Code({
  path,
  text,
  header,
}: {
  path: string
  text: string
  header: ReactNode
}) {
  const [selected, setSelected] = useState(readLines)
  const file = useMemo(() => ({ name: path, contents: text }), [path, text])
  // The options must keep their identity: new ones render the file again.
  const options = useMemo(() => {
    let scrolled = false
    return {
      ...codeOptions,
      enableLineSelection: true,
      onLineSelected: (r: SelectedLineRange | null) => {
        setSelected(r)
        writeLines(r)
      },
      // The lines are in a shadow root, out of reach of the fragment.
      onPostRender: (node: HTMLElement) => {
        const line = node.shadowRoot?.querySelector(
          `[data-line="${readLines()?.start}"]`
        )
        if (scrolled || !line) return
        scrolled = true
        requestAnimationFrame(() => line.scrollIntoView({ block: "center" }))
      },
    }
  }, [])
  // Without a header, the file is never rendered; this one replaces the
  // library's, which repeats the path.
  return (
    <File
      file={file}
      selectedLines={selected}
      options={options}
      renderCustomHeader={() => header}
      style={codeStyle}
    />
  )
}
