// Diffs and highlighted files, rendered by @pierre/diffs (diffs.com) with
// its Shiki highlighter.

import { parsePatchFiles, type FileDiffMetadata } from "@pierre/diffs"
import type { CSSProperties } from "react"

export interface DiffFile {
  diff: FileDiffMetadata
  binary: boolean
  adds: number
  dels: number
}

/** Splits `git diff-tree --patch` output into files. */
export function parseDiff(patch: string): DiffFile[] {
  // The parser keeps no mark for binary files, so find them in the patch.
  const binary = new Set<string>()
  for (const m of patch.matchAll(
    /^Binary files (?:a\/(.*)|\/dev\/null) and (?:b\/(.*)|\/dev\/null) differ$/gm
  )) {
    binary.add(m[2] ?? m[1])
  }
  return parsePatchFiles(patch).flatMap((p) =>
    p.files.map((diff) => ({
      diff,
      binary: binary.has(diff.name),
      adds: diff.hunks.reduce((n, h) => n + h.additionLines, 0),
      dels: diff.hunks.reduce((n, h) => n + h.deletionLines, 0),
    }))
  )
}

/** Options shared by diffs and files: system color scheme, the app's font. */
export const codeOptions = {
  themeType: "system",
  overflow: "scroll",
} as const

export const codeStyle = {
  "--diffs-font-family": "var(--font-mono)",
  "--diffs-header-font-family": "var(--font-sans)",
  "--diffs-font-size": "13px",
  "--diffs-line-height": "20px",
} as CSSProperties
