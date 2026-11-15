// Written by scripts/wire.ts from the types of the server's operations
// (crates/server/src/web/ops.rs); do not edit.

/**
 * A day with activity.
 */
export type ActivityDay = {
  /**
   * `YYYY-MM-DD`.
   */
  date: string
  lager: number
  github: number
}

export type ActivityTotals = { lager: number; github: number }

export type Blob = {
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

export type Commit = {
  oid: string
  parents: Array<string>
  author: Person
  committer: Person
  subject: string
}

export type CommitDetail = {
  generation: number
  commit: FullCommit
  diff: string
  truncated: boolean
}

export type Comparison = {
  generation: number
  base: string
  head: string
  merge_base: string
  /**
   * Newest first.
   */
  commits: Array<Commit>
  more: boolean
  diff: string
  truncated: boolean
}

/**
 * The answer of a change that has nothing to tell.
 */
export type Empty = Record<symbol, never>

export type EntryKind = "tree" | "file" | "symlink" | "submodule"

/**
 * A commit with its whole message.
 */
export type FullCommit = {
  message: string
  oid: string
  parents: Array<string>
  author: Person
  committer: Person
  subject: string
}

export type GithubAccount = {
  login: string
  /**
   * New repositories become mirrors.
   */
  auto: boolean
  /**
   * Why the last check failed.
   */
  auto_error: string | null
  /**
   * The repositories that the last check to skip any skipped, and why.
   */
  auto_skipped: string | null
}

export type GithubMirror = {
  /**
   * The repository's name here.
   */
  name: string
  /**
   * owner/name on GitHub.
   */
  github: string
  synced_ms: number | null
  error: string | null
}

/**
 * A GitHub repository as the settings page lists it.
 */
export type GithubRepo = {
  full_name: string
  name: string
  private: boolean
  description: string | null
  default_branch: string
  pushed_at: string | null
  created_at: string | null
}

export type GithubRepoList = { repos: Array<GithubRepo> }

export type GithubSignIn = { login: string }

/**
 * The user's GitHub account and mirrors, as the settings page shows them.
 */
export type GithubStatus = {
  /**
   * False without `LAGER_GITHUB_CLIENT_ID` on the server.
   */
  configured: boolean
  account: GithubAccount | null
  /**
   * The GitHub account that signs the user in
   * (docs/decisions/0031-github-sign-in.md).
   */
  sign_in: GithubSignIn | null
  mirrors: Array<GithubMirror>
}

export type Log = {
  generation: number
  oid: string
  commits: Array<Commit>
  more: boolean
}

export type Mergeable =
  "clean" | "conflict" | "up_to_date" | "source_missing" | "target_missing"

export type MirrorCreated = { mirror: GithubMirror }

/**
 * Who wrote or committed a commit.
 */
export type Person = {
  name: string
  email: string
  time: number
  /**
   * The user whose email this is, on commit authors.
   */
  user?: string
}

export type Profile = {
  user: string
  /**
   * GitHub's counts are included; mirrors count only there.
   */
  github: boolean
  totals: ActivityTotals
  /**
   * Days with activity in the last year, oldest first.
   */
  days: Array<ActivityDay>
}

export type Pull = {
  number: number
  title: string
  body: string
  author: string
  /**
   * Branch short names.
   */
  source: string
  target: string
  state: PullState
  /**
   * The source commit the server last recorded. Pages of open pull
   * requests show the source branch itself.
   */
  head_oid: string
  /**
   * The target commit the server last recorded; the merge commit after a
   * merge.
   */
  target_oid: string
  /**
   * Where the diff starts: the merge base of the recorded commits, kept
   * from before the merge.
   */
  merge_base: string
  merge_oid: string | null
  /**
   * `None` for a merge by a push.
   */
  merged_by: string | null
  version: number
  created_ms: number
  updated_ms: number
  closed_ms: number | null
  /**
   * The pull request's page on GitHub, for one copied from there. Its
   * authors are GitHub logins, and it cannot be changed here.
   */
  github_url: string | null
}

/**
 * A pull request after a change to it.
 */
export type PullChanged = { pull: Pull }

/**
 * Open and closed (with merged) pull requests of a repository.
 */
export type PullCounts = { open: number; closed: number }

export type PullDetail = {
  generation: number
  pull: Pull
  events: Array<PullEvent>
  /**
   * The comparison that shows the pull request's changes.
   */
  base: string
  head: string
  source_oid: string | null
  target_oid: string | null
  /**
   * `None` unless open, and for a copy from GitHub that no longer syncs.
   */
  mergeable: Mergeable | null
  conflicts: Array<string>
  can_comment: boolean
  can_merge: boolean
  can_close: boolean
}

export type PullEvent = {
  id: number
  kind: PullEventKind
  /**
   * `None` for the server.
   */
  author: string | null
  body: string | null
  old_oid: string | null
  oid: string | null
  created_ms: number
  /**
   * When a comment was last edited.
   */
  edited_ms: number | null
}

export type PullEventKind =
  "comment" | "push" | "closed" | "reopened" | "merged"

export type PullList = { pulls: Array<Pull>; counts: PullCounts; more: boolean }

export type PullState = "open" | "closed" | "merged"

export type RefInfo = {
  name: string
  oid: string
  time: number
  subject: string
}

export type Refs = {
  generation: number
  /**
   * The GitHub repository of a mirror.
   */
  mirror: string | null
  /**
   * Anyone may read it, also without signing in.
   */
  public: boolean
  head: string
  branches: Array<RefInfo>
  tags: Array<RefInfo>
}

/**
 * A repository after its creation or a change to its settings.
 */
export type RepoChanged = { repo: RepoSummary }

export type RepoList = { repos: Array<RepoSummary> }

export type RepoSummary = {
  name: string
  default_branch: string
  /**
   * Anyone may read it, also without signing in.
   */
  anonymous_read: boolean
  /**
   * The GitHub repository of a mirror.
   */
  mirror: string | null
}

export type Tree = {
  generation: number
  oid: string
  path: string
  entries: Array<TreeEntry>
  truncated: boolean
  last_commit: Commit | null
}

export type TreeEntry = {
  name: string
  kind: EntryKind
  mode: string
  oid: string
  size: number | null
}

/** The fields and the answer of every operation by name. */
export type Ops = {
  repos: { args: Record<symbol, never>; answer: RepoList }
  repo_create: {
    args: {
      name: string
      /**
       * Anyone may read it, also without signing in.
       */
      public?: boolean
    }
    answer: RepoChanged
  }
  repo_rename: { args: { repo: string; name: string }; answer: RepoChanged }
  repo_visibility: {
    args: {
      repo: string
      /**
       * Anyone may read it, also without signing in.
       */
      public: boolean
    }
    answer: RepoChanged
  }
  repo_delete: { args: { repo: string }; answer: Empty }
  refs: { args: { repo: string }; answer: Refs }
  log: {
    args: {
      repo: string
      rev: string
      path?: string
      skip?: number
      limit?: number
    }
    answer: Log
  }
  tree: { args: { repo: string; rev: string; path?: string }; answer: Tree }
  blob: { args: { repo: string; rev: string; path: string }; answer: Blob }
  commit: { args: { repo: string; rev: string }; answer: CommitDetail }
  pulls: {
    args: {
      repo: string
      /**
       * Closed and merged ones instead of open ones.
       */
      closed?: boolean
      skip?: number
      limit?: number
    }
    answer: PullList
  }
  pull: { args: { repo: string; number: number }; answer: PullDetail }
  compare: {
    args: { repo: string; base: string; head: string }
    answer: Comparison
  }
  pull_create: {
    args: {
      repo: string
      source: string
      target: string
      title: string
      body?: string
      /**
       * A UUID: a retry opens no second pull request.
       */
      request?: string
    }
    answer: PullChanged
  }
  pull_comment: {
    args: { repo: string; number: number; body: string; request?: string }
    answer: PullChanged
  }
  pull_close: { args: { repo: string; number: number }; answer: PullChanged }
  pull_reopen: { args: { repo: string; number: number }; answer: PullChanged }
  pull_merge: {
    args: {
      repo: string
      number: number
      /**
       * The source commit the page showed.
       */
      head: string
      squash?: boolean
      request: string
    }
    answer: PullChanged
  }
  profile: { args: { user: string }; answer: Profile }
  github: { args: Record<symbol, never>; answer: GithubStatus }
  github_repos: { args: Record<symbol, never>; answer: GithubRepoList }
  github_mirror: {
    args: {
      /**
       * `owner/name`.
       */
      github: string
    }
    answer: MirrorCreated
  }
  github_unmirror: { args: { name: string }; answer: Empty }
  github_disconnect: { args: Record<symbol, never>; answer: Empty }
  github_auto: { args: { on: boolean }; answer: Empty }
  github_unlink: { args: Record<symbol, never>; answer: Empty }
}
