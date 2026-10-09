# 1. Receive pushes with `git receive-pack` and a Rust `proc-receive` hook

Status: accepted. Date: September 27, 2026.

## Context

The design (ARCHITECTURE.md §7) requires that Rust, not Git, decides every
ref change and every push success response. Stage 1 (IMPLEMENTATION_PLAN.md
steps 5-7) requires a test of Git's `proc-receive` hook before further
server work. If the hook cannot give this control, the server must
implement the receive protocol in Rust.

## Decision

Use `git receive-pack --stateless-rpc` with a `proc-receive` hook for all
refs:

- Each cache repository sets `receive.procReceiveRefs = refs/`. Git does not
  change any ref itself. It reports success for a ref only when the hook
  reports `ok` for it.
- The hook is the `rustgit` binary, started through a symlink named
  `proc-receive`. It sends the commands to the controller through a private
  Unix socket, with an access token for each push that works once. The
  controller runs the complete push procedure, including the head CAS, and
  answers `ok` only after the CAS succeeds.
- `pre-receive` and `post-receive` are not used. User hooks are disabled
  (`core.hooksPath` points to a directory that holds only `proc-receive`).
- Git continues to receive, check (`receive.fsckObjects`), and store the
  objects. The server builds its own complete packs for storage with
  `git pack-objects`, so thin packs and loose objects need no special cases.

## Findings

Git moves received objects out of quarantine before it runs `proc-receive`.
Objects of a rejected push therefore stay in the cache. Reads do not expose
them:

- `upload-pack` runs on a separate read view that advertises only the refs of
  one accepted generation, with `uploadpack.allow*InWant = false`.
- Protocol version 2 is not passed to Git, because its `upload-pack`
  accepts any object ID in a want. Reads use protocol version 0 or 1.

A standard Git client cannot tell the difference. Protocol version 2 for
reads needs an object-access check, together with partial clone (a later
extension).

## Consequences

- Git 2.45 or later is required (`proc-receive` and transactions with
  `update-ref --stdin`). The Nix flake pins the Git version.
- A Rust implementation of the receive protocol is not necessary.
