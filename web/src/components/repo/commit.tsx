import type { FileDiffContentsLoader } from "@pierre/diffs"
import { FileDiff as PatchFileDiff } from "@pierre/diffs/react"
import { Link } from "@tanstack/react-router"
import { useCallback, useMemo, useState } from "react"

import { fetchBlob, useCommit } from "@/lib/api"
import { fullDate, short } from "@/lib/format"
import { href } from "@/lib/paths"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { QueryError } from "@/components/query-error"
import { Avatar } from "./avatar"
import { CopyButton } from "./clone"
import { codeOptions, codeStyle, parseDiff, type DiffFile } from "./diff"

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

export function CommitView({ repo, rev }: { repo: string; rev: string }) {
  const q = useCommit(repo, rev)
  const files = useMemo(() => (q.data ? parseDiff(q.data.diff) : []), [q.data])
  const [diffStyle, setDiffStyle] = useDiffStyle()
  // Expanding unchanged lines needs both whole files: the first parent's and
  // this commit's.
  const oid = q.data?.commit.oid
  const parent = q.data?.commit.parents[0]
  const loadDiffFiles = useCallback<FileDiffContentsLoader>(
    async (diff) => {
      const text = async (rev: string | undefined, path: string) => {
        if (!rev) throw new Error("no such revision")
        const b = await fetchBlob(repo, rev, path)
        if (b.text === null) throw new Error(`${path} is not shown as text`)
        return { name: path, contents: b.text }
      }
      const newFile = await text(oid, diff.name)
      if (diff.type === "rename-pure") return { oldFile: null, newFile }
      return {
        oldFile: await text(parent, diff.prevName ?? diff.name),
        newFile,
      }
    },
    [repo, oid, parent]
  )
  if (q.error) return <QueryError error={q.error} />
  if (!q.data) return <Skeleton className="h-96" />
  const { commit, truncated } = q.data
  const body = commit.message.split("\n").slice(1).join("\n").trim()
  const adds = files.reduce((n, f) => n + f.adds, 0)
  const dels = files.reduce((n, f) => n + f.dels, 0)

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
      <div className="flex flex-wrap items-center gap-3">
        <p className="text-sm text-muted-foreground">
          {files.length} {files.length === 1 ? "file" : "files"} changed,{" "}
          <span className="text-emerald-600">+{adds}</span>{" "}
          <span className="text-red-600">−{dels}</span>
          {commit.parents.length > 1 && " (against the first parent)"}
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
      </div>
      {files.map((f, i) => (
        <FileView
          key={i}
          file={f}
          diffStyle={diffStyle}
          loadDiffFiles={loadDiffFiles}
        />
      ))}
      {truncated && (
        <p className="rounded-lg border border-dashed p-6 text-center text-sm text-muted-foreground">
          The diff is too large to show completely.
        </p>
      )}
    </div>
  )
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
