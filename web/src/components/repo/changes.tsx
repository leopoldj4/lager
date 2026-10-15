import type { FileDiffContentsLoader } from "@pierre/diffs"
import { FileDiff as PatchFileDiff } from "@pierre/diffs/react"
import { useCallback, useEffect, useMemo, useRef, useState } from "react"

import { fetchBlob } from "@/lib/api"
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { codeOptions, codeStyle, parseDiff, type DiffFile } from "./diff"
import { DiffTree } from "./diff-tree"
import { SidePanelLayout } from "./side-panel"

type DiffStyle = "unified" | "split"

function useDiffStyle() {
  const [style, setStyle] = useState<DiffStyle>(() =>
    localStorage.getItem("diff-style") === "split" ? "split" : "unified"
  )
  const set = (s: DiffStyle) => {
    localStorage.setItem("diff-style", s)
    setStyle(s)
  }
  return [style, set] as const
}

/**
 * The files of a patch between two revisions, with a count of changed
 * lines. Expanding unchanged lines loads both whole files. With `tree`, a
 * tree of the files beside them leads to each.
 */
export function ChangedFiles({
  repo,
  diff,
  truncated,
  oldRev,
  newRev,
  note,
  tree,
}: {
  repo: string
  diff: string
  truncated: boolean
  /** Absent for a root commit. */
  oldRev: string | undefined
  newRev: string
  /** Appended to the summary. */
  note?: string
  /** Keeps the tree's width across visits; absent for no tree. */
  tree?: string
}) {
  const files = useMemo(() => parseDiff(diff), [diff])
  const [diffStyle, setDiffStyle] = useDiffStyle()
  const loadDiffFiles = useCallback<FileDiffContentsLoader>(
    async (d) => {
      const text = async (rev: string | undefined, path: string) => {
        if (!rev) throw new Error("no such revision")
        const b = await fetchBlob(repo, rev, path)
        if (b.text === null) throw new Error(`${path} is not shown as text`)
        return { name: path, contents: b.text }
      }
      const newFile = await text(newRev, d.name)
      if (d.type === "rename-pure") return { oldFile: null, newFile }
      return {
        oldFile: await text(oldRev, d.prevName ?? d.name),
        newFile,
      }
    },
    [repo, oldRev, newRev]
  )
  const adds = files.reduce((n, f) => n + f.adds, 0)
  const dels = files.reduce((n, f) => n + f.dels, 0)

  const views = useRef<(HTMLDivElement | null)[]>([])
  const [current, setCurrent] = useCurrentFile(views, files)

  const summary = (
    <>
      <p className="text-sm text-muted-foreground">
        {files.length} {files.length === 1 ? "file" : "files"} changed,{" "}
        <span className="text-emerald-600">+{adds}</span>{" "}
        <span className="text-red-600">−{dels}</span>
        {note}
      </p>
      <Tabs
        value={diffStyle}
        onValueChange={(v) => setDiffStyle(v as DiffStyle)}
        className="ml-auto"
      >
        <TabsList>
          <TabsTrigger value="unified">Unified</TabsTrigger>
          <TabsTrigger value="split">Split</TabsTrigger>
        </TabsList>
      </Tabs>
    </>
  )
  const diffs = (
    <div className="grid grid-cols-1 gap-6">
      {files.map((f, i) => (
        <div
          key={i}
          ref={(el) => {
            views.current[i] = el
          }}
          // Below the sticky header on phones.
          className="max-md:scroll-mt-14"
        >
          <FileView
            file={f}
            diffStyle={diffStyle}
            loadDiffFiles={loadDiffFiles}
          />
        </div>
      ))}
      {truncated && (
        <p className="rounded-lg border border-dashed p-6 text-center text-sm text-muted-foreground">
          The diff is too large to show completely.
        </p>
      )}
    </div>
  )

  if (!tree) {
    return (
      <div className="grid grid-cols-1 gap-6">
        <div className="flex flex-wrap items-center gap-3">{summary}</div>
        {diffs}
      </div>
    )
  }
  return (
    <SidePanelLayout
      id={tree}
      title="Changed files"
      toolbar={summary}
      side={(onNavigate) => (
        <DiffTree
          files={files}
          current={current}
          onSelect={(i) => {
            onNavigate?.()
            // The last files may not reach the top, so mark the choice.
            setCurrent(i)
            views.current[i]?.scrollIntoView({ block: "start" })
          }}
        />
      )}
    >
      {diffs}
    </SidePanelLayout>
  )
}

/** The index of the topmost file diff in view, until another is chosen. */
function useCurrentFile(
  views: React.RefObject<(HTMLDivElement | null)[]>,
  files: DiffFile[]
) {
  const [current, setCurrent] = useState<number>()
  useEffect(() => {
    const visible = new Set<number>()
    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          const i = views.current.indexOf(e.target as HTMLDivElement)
          if (e.isIntersecting) visible.add(i)
          else visible.delete(i)
        }
        setCurrent(visible.size ? Math.min(...visible) : undefined)
      },
      // Not under the sticky header on phones.
      { rootMargin: "-64px 0px 0px 0px" }
    )
    for (const el of views.current.slice(0, files.length)) {
      if (el) io.observe(el)
    }
    return () => io.disconnect()
  }, [views, files])
  return [current, setCurrent] as const
}

function FileView({
  file,
  diffStyle,
  loadDiffFiles,
}: {
  file: DiffFile
  diffStyle: DiffStyle
  loadDiffFiles: FileDiffContentsLoader
}) {
  const { diff } = file
  // The options must keep their identity: new ones render the diff again.
  const options = useMemo(
    () => ({ ...codeOptions, diffStyle, loadDiffFiles }),
    [diffStyle, loadDiffFiles]
  )
  const note = file.binary
    ? "Binary file changed."
    : diff.hunks.length > 0
      ? null
      : diff.type === "rename-pure"
        ? "File renamed without changes."
        : diff.prevMode && diff.prevMode !== diff.mode
          ? `File mode changed from ${diff.prevMode} to ${diff.mode}.`
          : "No content changes."
  return (
    <div className="overflow-hidden rounded-lg border">
      <PatchFileDiff fileDiff={diff} options={options} style={codeStyle} />
      {note && (
        <p className="border-t p-4 text-sm text-muted-foreground">{note}</p>
      )}
    </div>
  )
}
