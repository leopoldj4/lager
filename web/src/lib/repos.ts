// Repositories and their contents: read queries over the live connection,
// cached with TanStack Query. Every query of a repository has a key that
// starts with ["repo", name], so a push to it refreshes all of them.

import { useEffect, useState } from "react"
import {
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query"
import { live, type Change } from "./live"
import { queryClient, useLive } from "./query"

export function useRepos() {
  const { user } = useLive() ?? {}
  return useQuery({
    queryKey: ["repos", user],
    queryFn: () => live.request({ op: "repos" }),
  })
}

/** Creates an empty repository with the chosen visibility. */
export function useCreateRepo() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (repo: { name: string; public: boolean }) =>
      live.request({ op: "repo_create", ...repo }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["repos"] }),
  })
}

type SettingsOp = "repo_rename" | "repo_visibility" | "repo_delete"

/**
 * Changes a repository's settings: `repo_rename` (with the new full
 * `name`), `repo_visibility` (with `public`), or `repo_delete`.
 */
export function useRepoSettingsChange(repo: string) {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (change: Change<SettingsOp>) =>
      live.request<SettingsOp>({ ...change, repo }),
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
    queryFn: () => live.request({ op: "refs", repo }),
  })
}

export function useTree(repo: string, rev: string, path: string) {
  return useQuery({
    queryKey: ["repo", repo, "tree", rev, path],
    queryFn: () => live.request({ op: "tree", repo, rev, path }),
    enabled: rev !== "",
  })
}

const blobQuery = (repo: string, rev: string, path: string) => ({
  queryKey: ["repo", repo, "blob", rev, path],
  queryFn: () => live.request({ op: "blob", repo, rev, path }),
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
      live.request({ op: "log", repo, rev, path, skip: pageParam, limit }),
    initialPageParam: 0,
    getNextPageParam: (last, pages) =>
      last.more ? pages.length * limit : undefined,
    enabled: rev !== "",
  })
}

export function useCommit(repo: string, rev: string) {
  return useQuery({
    queryKey: ["repo", repo, "commit", rev],
    queryFn: () => live.request({ op: "commit", repo, rev }),
    // A commit never changes.
    staleTime: Infinity,
  })
}
