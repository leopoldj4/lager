// Read queries over the live connection, cached with TanStack Query. Every
// query of a repository has a key that starts with ["repo", name], so a
// push to it refreshes all of them.

import { useEffect, useState, useSyncExternalStore } from "react"
import {
  QueryClient,
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query"
import { live, LiveError } from "./live"

export interface Person {
  name: string
  email: string
  time: number
}

export interface Commit {
  oid: string
  parents: string[]
  author: Person
  committer: Person
  subject: string
  message?: string
}

export interface RepoSummary {
  name: string
  default_branch: string
  anonymous_read: boolean
  /** The GitHub repository of a read-only mirror. */
  mirror: string | null
}

export interface RefInfo {
  name: string
  oid: string
  time: number
  subject: string
}

export interface Refs {
  generation: number
  /** The GitHub repository of a read-only mirror. */
  mirror: string | null
  /** Anyone may read it, also without signing in. */
  public: boolean
  head: string
  branches: RefInfo[]
  tags: RefInfo[]
}

export interface TreeEntry {
  name: string
  kind: "tree" | "file" | "symlink" | "submodule"
  mode: string
  oid: string
  size: number | null
}

export interface Tree {
  generation: number
  oid: string
  path: string
  entries: TreeEntry[]
  truncated: boolean
  last_commit: Commit | null
}

export interface Blob {
  generation: number
  oid: string
  path: string
  blob: string
  size: number
  binary: boolean
  too_large: boolean
  text: string | null
  last_commit: Commit | null
}

export interface Log {
  generation: number
  oid: string
  commits: Commit[]
  more: boolean
}

export interface CommitDetail {
  generation: number
  commit: Commit & { message: string }
  diff: string
  truncated: boolean
}

// Pull requests (docs/decisions/0019-pull-requests.md).

export type PullState = "open" | "closed" | "merged"

export interface Pull {
  number: number
  title: string
  body: string
  author: string
  source: string
  target: string
  state: PullState
  head_oid: string
  target_oid: string
  merge_base: string
  merge_oid: string | null
  /** null for a merge by a push. */
  merged_by: string | null
  version: number
  created_ms: number
  updated_ms: number
  closed_ms: number | null
}

export interface PullEvent {
  id: number
  kind: "comment" | "push" | "closed" | "reopened" | "merged"
  /** null for the server. */
  author: string | null
  body: string | null
  old_oid: string | null
  oid: string | null
  created_ms: number
}

export interface PullList {
  pulls: Pull[]
  counts: { open: number; closed: number }
  more: boolean
}

export type Mergeable =
  "clean" | "conflict" | "up_to_date" | "source_missing" | "target_missing"

export interface PullDetail {
  generation: number
  pull: Pull
  events: PullEvent[]
  /** The comparison that shows the pull request's changes. */
  base: string
  head: string
  source_oid: string | null
  target_oid: string | null
  /** null unless open. */
  mergeable: Mergeable | null
  conflicts: string[]
  can_comment: boolean
  can_merge: boolean
  can_close: boolean
}

export interface Comparison {
  generation: number
  base: string
  head: string
  merge_base: string
  /** Newest first. */
  commits: Commit[]
  more: boolean
  diff: string
  truncated: boolean
}

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 60_000,
      retry: (n, e) =>
        !(e instanceof LiveError && e.code !== "unavailable") && n < 3,
    },
  },
})

export function useLive() {
  return useSyncExternalStore(
    live?.subscribe ?? (() => () => {}),
    () => live?.getState(),
    () => null
  )
}

export function useRepos() {
  const { user } = useLive() ?? {}
  return useQuery({
    queryKey: ["repos", user],
    queryFn: () => live.request<{ repos: RepoSummary[] }>({ op: "repos" }),
  })
}

/** Creates an empty repository with the chosen visibility. */
export function useCreateRepo() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (repo: { name: string; public: boolean }) =>
      live.request<{ repo: RepoSummary }>({ op: "repo_create", ...repo }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["repos"] }),
  })
}

/**
 * Changes a repository's settings: `repo_rename` (with the new full
 * `name`), `repo_visibility` (with `public`), or `repo_delete`.
 */
export function useRepoSettingsChange(repo: string) {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (change: { op: string; [k: string]: unknown }) =>
      live.request<{ repo?: RepoSummary }>({ ...change, repo }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["repos"] })
      qc.invalidateQueries({ queryKey: ["repo", repo] })
    },
  })
}

/**
 * Watches a repository while the page shows it. Answers the time of the
 * last push seen, to show that the page changed.
 */
export function useWatch(repo: string) {
  const qc = useQueryClient()
  const [pushedAt, setPushedAt] = useState<number | null>(null)
  useEffect(() => {
    const refresh = () => qc.invalidateQueries({ queryKey: ["repo", repo] })
    return live.watch(repo, {
      onChange: () => {
        setPushedAt(Date.now())
        refresh()
      },
      onResync: refresh,
      onPull: (number) => {
        qc.invalidateQueries({ queryKey: ["repo", repo, "pulls"] })
        qc.invalidateQueries({ queryKey: ["repo", repo, "pull", number] })
      },
    })
  }, [qc, repo])
  return pushedAt
}

export function useRefs(repo: string) {
  return useQuery({
    queryKey: ["repo", repo, "refs"],
    queryFn: () => live.request<Refs>({ op: "refs", repo }),
  })
}

export function useTree(repo: string, rev: string, path: string) {
  return useQuery({
    queryKey: ["repo", repo, "tree", rev, path],
    queryFn: () => live.request<Tree>({ op: "tree", repo, rev, path }),
    enabled: rev !== "",
  })
}

const blobQuery = (repo: string, rev: string, path: string) => ({
  queryKey: ["repo", repo, "blob", rev, path],
  queryFn: () => live.request<Blob>({ op: "blob", repo, rev, path }),
})

export function useBlob(
  repo: string,
  rev: string,
  path: string,
  enabled = true
) {
  return useQuery({
    ...blobQuery(repo, rev, path),
    enabled: enabled && rev !== "",
  })
}

export function fetchBlob(repo: string, rev: string, path: string) {
  return queryClient.fetchQuery(blobQuery(repo, rev, path))
}

export function useLog(repo: string, rev: string, path: string) {
  const limit = 30
  return useInfiniteQuery({
    queryKey: ["repo", repo, "log", rev, path],
    queryFn: ({ pageParam }) =>
      live.request<Log>({ op: "log", repo, rev, path, skip: pageParam, limit }),
    initialPageParam: 0,
    getNextPageParam: (last, pages) =>
      last.more ? pages.length * limit : undefined,
    enabled: rev !== "",
  })
}

export function useCommit(repo: string, rev: string) {
  return useQuery({
    queryKey: ["repo", repo, "commit", rev],
    queryFn: () => live.request<CommitDetail>({ op: "commit", repo, rev }),
    // A commit never changes.
    staleTime: Infinity,
  })
}

export function usePulls(repo: string, closed: boolean, enabled = true) {
  const limit = 30
  return useInfiniteQuery({
    enabled,
    queryKey: ["repo", repo, "pulls", closed],
    queryFn: ({ pageParam }) =>
      live.request<PullList>({
        op: "pulls",
        repo,
        closed,
        skip: pageParam,
        limit,
      }),
    initialPageParam: 0,
    getNextPageParam: (last, pages) =>
      last.more ? pages.length * limit : undefined,
  })
}

/** Open and closed pull requests, for the repository's tabs. */
export function usePullCounts(repo: string) {
  return useQuery({
    queryKey: ["repo", repo, "pulls", "counts"],
    queryFn: () => live.request<PullList>({ op: "pulls", repo, limit: 1 }),
    select: (d) => d.counts,
  })
}

export function usePull(repo: string, number: number) {
  return useQuery({
    queryKey: ["repo", repo, "pull", number],
    queryFn: () => live.request<PullDetail>({ op: "pull", repo, number }),
  })
}

/** The commits and changes of `head` since its merge base with `base`. */
export function useCompare(repo: string, base: string, head: string) {
  return useQuery({
    queryKey: ["repo", repo, "compare", base, head],
    queryFn: () =>
      live.request<Comparison>({ op: "compare", repo, base, head }),
    enabled: base !== "" && head !== "",
  })
}

/** A UUID for retries of a change; also outside secure contexts. */
export function requestId() {
  const b = crypto.getRandomValues(new Uint8Array(16))
  b[6] = (b[6] & 0x0f) | 0x40
  b[8] = (b[8] & 0x3f) | 0x80
  const h = [...b].map((x) => x.toString(16).padStart(2, "0")).join("")
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`
}

/**
 * Changes a pull request: `pull_create`, `pull_comment`, `pull_close`,
 * `pull_reopen`, or `pull_merge`. Pages refresh from the change event too;
 * refreshing here shows the change without waiting for it.
 */
export function usePullChange(repo: string) {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (change: { op: string; [k: string]: unknown }) =>
      live.request<{ pull: Pull }>({ ...change, repo }),
    onSuccess: ({ pull }) => {
      qc.invalidateQueries({ queryKey: ["repo", repo, "pulls"] })
      qc.invalidateQueries({ queryKey: ["repo", repo, "pull", pull.number] })
    },
  })
}

// GitHub mirrors (docs/decisions/0020-github-mirrors.md).

export interface GithubMirror {
  name: string
  github: string
  synced_ms: number | null
  error: string | null
}

export interface GithubStatus {
  /** False without [github] in the server configuration. */
  configured: boolean
  account: { login: string } | null
  mirrors: GithubMirror[]
}

export interface GithubRepo {
  full_name: string
  name: string
  private: boolean
  description: string | null
  default_branch: string
  pushed_at: string | null
}

export function useGithub() {
  const { user } = useLive() ?? {}
  return useQuery({
    queryKey: ["github", user],
    queryFn: () => live.request<GithubStatus>({ op: "github" }),
    enabled: !!user,
    // Shows syncs as they finish.
    refetchInterval: 5_000,
  })
}

export function useGithubRepos(enabled: boolean) {
  const { user } = useLive() ?? {}
  return useQuery({
    queryKey: ["github", user, "repos"],
    queryFn: () =>
      live.request<{ repos: GithubRepo[] }>({ op: "github_repos" }),
    enabled: !!user && enabled,
    staleTime: 5 * 60_000,
  })
}

/** `github_mirror`, `github_unmirror`, or `github_disconnect`. */
export function useGithubChange() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (change: { op: string; [k: string]: unknown }) =>
      live.request<unknown>(change),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["github"] })
      qc.invalidateQueries({ queryKey: ["repos"] })
    },
  })
}

export async function signIn(user: string, password: string) {
  const r = await fetch("/-/api/session", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ user, password }),
  })
  if (!r.ok) {
    const body = await r.json().catch(() => ({}))
    throw new LiveError(
      body.code ?? "error",
      body.message ?? `sign-in failed (${r.status})`
    )
  }
  live.reconnect()
  queryClient.clear()
}

export async function signOut() {
  await fetch("/-/api/session", { method: "DELETE" })
  live.reconnect()
  queryClient.clear()
}
