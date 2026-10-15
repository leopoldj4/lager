// Page addresses. Repository names can contain "/", so a "-" segment, which
// no repository name contains, separates the name from the page:
//   /<repo>                          the overview
//   /<repo>/-/tree/<rev>/<path>      a directory; without <rev>, the default branch
//   /<repo>/-/blob/<rev>/<path>      a file
//   /<repo>/-/commits/<rev>/<path>   history
//   /<repo>/-/commit/<oid>           one commit
//   /<repo>/-/branches, /-/tags
//   /<repo>/-/raw/<rev>/<path>       a file's bytes, from the server
//   /<repo>/-/pulls, /-/pulls/closed    pull requests
//   /<repo>/-/pull/<n>[/commits|/files]  one pull request
//   /<repo>/-/compare/<base>...<head>   a comparison, to open a pull request
// Branch names can contain "/" too; the page splits <rev>/<path> with the
// repository's refs.

export type View =
  | "overview"
  | "tree"
  | "blob"
  | "commits"
  | "commit"
  | "branches"
  | "tags"
  | "pulls"
  | "pull"
  | "compare"

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
  "pulls",
  "pull",
  "compare",
])

export function parse(splat: string): Location | null {
  const segs = splat.split("/").filter(Boolean)
  const dash = segs.indexOf("-")
  const repoSegs = dash < 0 ? segs : segs.slice(0, dash)
  if (repoSegs.length === 0) return null
  const repo = repoSegs.join("/")
  if (dash < 0) return { repo, view: "overview", rest: [] }
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

/** The address of a file's bytes, which the server answers. */
export function rawHref(repo: string, rev: string, path: string) {
  const enc = (s: string) => s.split("/").map(encodeURIComponent).join("/")
  return `/${repo}/-/raw/${enc(rev)}/${enc(path)}`
}

/**
 * The repository path a relative link in the file at `from` names, or null
 * when it leaves the repository. A leading "/" starts at the root.
 */
export function resolvePath(from: string, target: string): string | null {
  const parts = target.startsWith("/") ? [] : from.split("/").slice(0, -1)
  for (const seg of target.split("/")) {
    if (seg === "" || seg === ".") continue
    if (seg !== "..") parts.push(seg)
    else if (parts.pop() === undefined) return null
  }
  return parts.join("/")
}

/** Splits <base>...<head>; ref names never contain "..". */
export function splitCompare(rest: string[]) {
  const [base = "", head = ""] = rest.join("/").split("...")
  return { base, head }
}
