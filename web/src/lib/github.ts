// GitHub mirrors (docs/decisions/0020-github-mirrors.md).

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { live, type Request } from "./live"
import { useLive } from "./query"

export function useGithub() {
  const { user } = useLive() ?? {}
  return useQuery({
    queryKey: ["github", user],
    queryFn: () => live.request({ op: "github" }),
    enabled: !!user,
    // Shows syncs as they finish.
    refetchInterval: 5_000,
  })
}

export function useGithubRepos(enabled: boolean) {
  const { user } = useLive() ?? {}
  return useQuery({
    queryKey: ["github", user, "repos"],
    queryFn: () => live.request({ op: "github_repos" }),
    enabled: !!user && enabled,
    staleTime: 5 * 60_000,
  })
}

type GithubChangeOp =
  | "github_mirror"
  | "github_unmirror"
  | "github_auto"
  | "github_disconnect"
  | "github_unlink"

/**
 * `github_mirror`, `github_unmirror`, `github_auto`, `github_disconnect`, or
 * `github_unlink`.
 */
export function useGithubChange() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (change: Request<GithubChangeOp>) => live.request(change),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["github"] })
      qc.invalidateQueries({ queryKey: ["repos"] })
    },
  })
}
