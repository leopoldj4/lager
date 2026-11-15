import type { FileDiffContentsLoader } from "@pierre/diffs"
import { FileDiff as PatchFileDiff } from "@pierre/diffs/react"
import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { createPortal } from "react-dom"

import { fetchBlob } from "@/lib/repos"
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { codeOptions, codeStyle, parseDiff, type DiffFile } from "./diff"
import { DiffTree } from "./diff-tree"

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
 * tree of the files, drawn into that element elsewhere on the page, leads
 * to each.
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
  /** Where the tree of the files goes; absent for no tree. */
  tree?: HTMLElement | null
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
        <span className="font-mono text-xs text-added">+{adds}</span>{" "}
        <span className="font-mono text-xs text-removed">−{dels}</span>
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
          // Below the sticky page header.
          className="scroll-mt-14"
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

  return (
    <div className="grid grid-cols-1 gap-6">
      <div className="flex flex-wrap items-center gap-3">{summary}</div>
      {diffs}
      {tree &&
        createPortal(
          <DiffTree
            files={files}
            current={current}
            onSelect={(i) => {
              // The last files may not reach the top, so mark the choice.
              setCurrent(i)
              views.current[i]?.scrollIntoView({ block: "start" })
            }}
          />,
          tree
        )}
    </div>
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
      // Not under the sticky page header.
      { rootMargin: "-56px 0px 0px 0px" }
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
    <div className="overflow-hidden rounded-[9px] border">
      <PatchFileDiff fileDiff={diff} options={options} style={codeStyle} />
      {note && (
        <p className="border-t p-4 text-sm text-muted-foreground">{note}</p>
      )}
    </div>
  )
}
