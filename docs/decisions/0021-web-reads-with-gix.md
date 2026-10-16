# 21. Web reads of objects and refs with gix

Status: accepted; implemented in `crates/server/src/web/read.rs` and
`crates/server/src/repository/mod.rs`. Date: October 16, 2026.

## Context

Each read query of the web interface (decision 14) started Git processes
one after another: a `tree` started four (`rev-parse`, `cat-file`,
`ls-tree`, `log`), a `blob` four, and the object ID of a commit a fifth
for `for-each-ref --contains`. On the deployment a Git process costs about
40 ms, so `tree` and `blob` took about 190 ms on the server, nearly all
of it starting processes. Opening a repository runs several of them, and
every click in the code view waits for one.

## Decision

**Objects and refs in the server.** The server reads a read view with
[gix](https://docs.rs/gix) (`=0.89.0`, without network or worktree
features) instead of Git for: `refs`, the resolution of a `rev` with its
check that a ref reaches a commit, `tree`, `blob`, the raw route, the
history of a commit without a path, the commit of `commit`, the last commit
of the root directory, and the commits of a pull request's refs. A read
view opens once per generation, with no system, global, or environment
configuration, as the Git processes run; the reads run on Tokio's blocking
threads.

**Git stays for the rest.** Diffs (`commit`, `compare`, pull requests),
merge bases, merges, the history of a path, and the last commit of a path
still run Git. Git's simplification of merges decides which commits a
path's history shows; the pages show Git's answer. Clones, fetches, pushes,
packs, and ref changes are unchanged (§7 of ARCHITECTURE.md).

**Limits.** The limits of decision 14 hold: trees stop at 5000 entries,
files above 1 MiB are answered without contents, and the raw route refuses
files above 20 MiB. A read still takes a Git process slot. Walks over
history check the query's 30 seconds at each commit and answer
`unavailable` after it. The history of a commit is ordered by commit time,
newest first, as `git log`; commits with equal times can be ordered
differently.

## Consequences

- `tree` and `blob` start at most one Git process, for the last commit of
  a path, which runs beside the gix read. The root directory starts none.
- gix reads in the server process. Limits on Git processes
  (`GIT_CPU_SECS` and `GIT_MEMORY_BYTES` in `crates/core/src/limits.rs`,
  0 today) would not apply to them, and a read past its time cannot be
  stopped between two checks. The size limits above and the process slots
  bound them instead.
- The server crate depends on gix and its crates; versions are fixed like
  the other dependencies.
- A later decision can move diffs or the history of a path to gix; their
  output must then match what the pages parse today.
