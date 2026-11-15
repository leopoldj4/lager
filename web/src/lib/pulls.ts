// Pull requests (docs/decisions/0019-pull-requests.md). Their queries have
// keys under ["repo", name], as the repository's others (./repos).

import {
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query"
import { live, type Change } from "./live"

export function usePulls(repo: string, closed: boolean, enabled = true) {
  const limit = 30
  return useInfiniteQuery({
    enabled,
    queryKey: ["repo", repo, "pulls", closed],
    queryFn: ({ pageParam }) =>
      live.request({
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
    queryFn: () => live.request({ op: "pulls", repo, limit: 1 }),
    select: (d) => d.counts,
  })
}

export function usePull(repo: string, number: number) {
  return useQuery({
    queryKey: ["repo", repo, "pull", number],
    queryFn: () => live.request({ op: "pull", repo, number }),
  })
}

/** The commits and changes of `head` since its merge base with `base`. */
export function useCompare(repo: string, base: string, head: string) {
  return useQuery({
    queryKey: ["repo", repo, "compare", base, head],
    queryFn: () => live.request({ op: "compare", repo, base, head }),
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

type PullChangeOp =
  "pull_create" | "pull_comment" | "pull_close" | "pull_reopen" | "pull_merge"

/**
 * Changes a pull request: `pull_create`, `pull_comment`, `pull_close`,
 * `pull_reopen`, or `pull_merge`. Pages refresh from the change event too;
 * refreshing here shows the change without waiting for it.
 */
export function usePullChange(repo: string) {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (change: Change<PullChangeOp>) =>
      live.request<PullChangeOp>({ ...change, repo }),
    onSuccess: ({ pull }) => {
      qc.invalidateQueries({ queryKey: ["repo", repo, "pulls"] })
      qc.invalidateQueries({ queryKey: ["repo", repo, "pull", pull.number] })
    },
  })
}
