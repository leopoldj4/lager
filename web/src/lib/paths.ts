// Page addresses. Repository names can contain "/", so a "-" segment, which
// no repository name contains, separates the name from the page:
//   /<repo>                          the default branch
//   /<repo>/-/tree/<rev>/<path>      a directory
//   /<repo>/-/blob/<rev>/<path>      a file
//   /<repo>/-/commits/<rev>/<path>   history
//   /<repo>/-/commit/<oid>           one commit
//   /<repo>/-/branches, /-/tags
// Branch names can contain "/" too; the page splits <rev>/<path> with the
// repository's refs.

export type View = "tree" | "blob" | "commits" | "commit" | "branches" | "tags"

export interface Location {
  repo: string
  view: View
  /** <rev>/<path>, still joined. */
  rest: string[]
}

const views = new Set<string>([
  "tree",
  "blob",
  "commits",
  "commit",
  "branches",
  "tags",
])

export function parse(splat: string): Location | null {
  const segs = splat.split("/").filter(Boolean)
  const dash = segs.indexOf("-")
  const repoSegs = dash < 0 ? segs : segs.slice(0, dash)
  if (repoSegs.length === 0) return null
  const repo = repoSegs.join("/")
  if (dash < 0) return { repo, view: "tree", rest: [] }
  const view = segs[dash + 1]
  if (!views.has(view)) return null
  return { repo, view: view as View, rest: segs.slice(dash + 2) }
}

/** Splits <rev>/<path> at the longest branch or tag name. */
export function splitRev(rest: string[], refs: string[] | undefined) {
  if (rest.length === 0) return { rev: "", path: "" }
  const names = new Set(refs ?? [])
  for (let n = rest.length; n > 1; n--) {
    const rev = rest.slice(0, n).join("/")
    if (names.has(rev)) return { rev, path: rest.slice(n).join("/") }
  }
  return { rev: rest[0], path: rest.slice(1).join("/") }
}

/** The splat parameter for a page; the router encodes it. */
export function href(repo: string, view?: View, rev?: string, path?: string) {
  let s = repo
  if (!view) return s
  s += `/-/${view}`
  if (rev) s += `/${rev}`
  if (path) s += `/${path}`
  return s
}
