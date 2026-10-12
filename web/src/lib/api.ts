// Read queries over the live connection, cached with TanStack Query. Every
// query of a repository has a key that starts with ["repo", name], so a
// push to it refreshes all of them.

import { useEffect, useState, useSyncExternalStore } from "react"
import {
  QueryClient,
  useInfiniteQuery,
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
}

export interface RefInfo {
  name: string
  oid: string
  time: number
  subject: string
}

export interface Refs {
  generation: number
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

export function useBlob(
  repo: string,
  rev: string,
  path: string,
  enabled = true
) {
  return useQuery({
    queryKey: ["repo", repo, "blob", rev, path],
    queryFn: () => live.request<Blob>({ op: "blob", repo, rev, path }),
    enabled: enabled && rev !== "",
  })
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
