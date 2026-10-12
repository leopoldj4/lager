// Unified diffs from `git diff-tree --patch`, split into files and lines.

export interface DiffLine {
  kind: "add" | "del" | "ctx" | "hunk"
  text: string
  old?: number
  new?: number
}

export interface DiffFile {
  path: string
  oldPath?: string
  status?: "added" | "deleted" | "renamed" | "mode changed"
  binary: boolean
  adds: number
  dels: number
  lines: DiffLine[]
}

export function parseDiff(diff: string): DiffFile[] {
  const files: DiffFile[] = []
  let f: DiffFile | null = null
  let oldNo = 0
  let newNo = 0
  let inHunk = false
  for (const line of diff.split("\n")) {
    if (line.startsWith("diff --git ")) {
      const m = /^diff --git a\/(.*) b\/(.*)$/.exec(line)
      f = {
        path: m?.[2] ?? line.slice(11),
        binary: false,
        adds: 0,
        dels: 0,
        lines: [],
      }
      files.push(f)
      inHunk = false
      continue
    }
    if (!f) continue
    if (!inHunk) {
      if (line.startsWith("new file mode")) f.status = "added"
      else if (line.startsWith("deleted file mode")) f.status = "deleted"
      else if (line.startsWith("rename from ")) {
        f.oldPath = line.slice(12)
        f.status = "renamed"
      } else if (line.startsWith("rename to ")) f.path = line.slice(10)
      else if (line.startsWith("old mode")) f.status ??= "mode changed"
      else if (line.startsWith("Binary files ")) f.binary = true
    }
    const hunk = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line)
    if (hunk) {
      oldNo = Number(hunk[1])
      newNo = Number(hunk[2])
      inHunk = true
      f.lines.push({ kind: "hunk", text: line })
      continue
    }
    if (!inHunk) continue
    if (line.startsWith("+")) {
      f.adds++
      f.lines.push({ kind: "add", text: line.slice(1), new: newNo++ })
    } else if (line.startsWith("-")) {
      f.dels++
      f.lines.push({ kind: "del", text: line.slice(1), old: oldNo++ })
    } else if (line.startsWith(" ")) {
      f.lines.push({
        kind: "ctx",
        text: line.slice(1),
        old: oldNo++,
        new: newNo++,
      })
    }
  }
  return files
}
