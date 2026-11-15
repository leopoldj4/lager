# 35. The web interface's types come from the server's

Status: accepted; implemented in `crates/server/src/web/ops.rs`,
`scripts/wire.ts`, and `web/src/lib/wire.ts`. Date: November 14, 2026.

## Context

The pages send operations over the live connection (decisions 13 and 14):
read queries and changes, each a `req` message with an `op` and its
fields. The server built every answer as untyped JSON, and the web
interface declared matching types by hand. Nothing checked that the two
agreed, or that a page sent the fields an operation takes.

## Decision

**One type per operation.** An operation is a struct with the fields of
its request. It implements `Op`, which names the type of its answer and
answers it. The compiler therefore ties each request to its answer.

**One table.** `ops.rs` lists every operation by its name on the wire. A
macro writes the dispatcher and the TypeScript export from that list, so
an operation cannot be in one and missing in the other. An unknown name or
a wrong field answers `bad_request` with the request's `id`.

**Generated types.** [`ts-rs`](https://docs.rs/ts-rs/12.0.1/ts_rs/) derives
a TypeScript type from each Rust type, with its doc comments. `scripts/wire.ts`
writes `web/src/lib/wire.ts`: the types of the answers and `Ops`, the
fields and the answer of every operation by name. `Live.request` takes its
types from `Ops`, so a page that names an operation gets its fields
checked and its answer typed.

**Checked in.** The file is in the repository: the image builds the web
interface without Rust, and a change to the contract shows in review.
`scripts/wire.ts --check` fails when the file is out of date.

**Numbers.** 64-bit integers are TypeScript numbers. The counts and times
in the answers stay far below 2⁵³.

**Optional fields.** A field of a request that may be missing is an
`Option` in Rust and optional in TypeScript; the operation applies the
default.

## Consequences

- The core crate depends on `ts-rs` too: pull requests and GitHub accounts
  go on the wire as the database answers them.
- `scripts/wire.ts` must run after a change to an operation or to a type
  in an answer. Nothing runs the check automatically yet.
- The settings requests over plain HTTP (access tokens, SSH keys, email
  addresses, passkeys) and the messages of the live connection itself
  still have hand-written types on both sides.
- Commits read from Git are `Unlinked` until `Git::link` looks up their
  authors among the users, so an answer cannot hold commits whose authors
  were not looked up.
- The declarations in `wire.ts` are sorted by name: `ts-rs` visits the
  types in an order that changes from build to build.
