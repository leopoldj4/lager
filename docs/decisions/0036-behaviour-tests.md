# 36. Behaviour tests run against several servers

Status: accepted; implemented in `tests/`. Date: November 15, 2026.

## Context

lager had no tests. Its main promise is that any number of servers share
one storage service and answer alike, and nothing checked it: local mode
runs one server. Parts of the server keep state in the process (locks,
queues, caches), and whether each is safe with several servers was known
only by reading the code.

## Decision

**From outside.** The tests use the server as its users do: the `git`
program over HTTP and SSH, the live connection, and the administration
commands. They know nothing of the server's code, so they stay valid when
it changes.

**Against a cluster.** The harness starts SeaweedFS, PostgreSQL, and NATS
from `scripts/local.compose.yaml`, and several `lager-server` processes of
the checkout's build that share them. `LAGER_TEST_SERVERS` sets how many;
the default is 2.

**Every call to the next server.** Clients take `cluster().next()` for
each Git command and each connection. A test that pushes and then reads
therefore pushes to one server and reads from another without saying so.
The same tests run with one server and with three; a test that passes
with one and fails with three shows state that a server keeps to itself.

**New data for every test.** Each test makes its own users and
repositories with random names, and every run has its own database and
storage prefix. Tests do not depend on each other or on their order.

**Outages from the test.** Each server reaches each service through its
own relay in the test process (`tests/harness/link.ts`). A test cuts and
restores the connection of one server to storage, the database, or NATS,
and kills and starts servers, with or without their cache.

**Known faults are tests.** A behaviour that is wrong today is a test
marked `test.failing`, with the cause above it. The suite stays green, and
the test turns red when the fault is fixed, which is the moment to remove
the mark. Faults that only several servers show are marked only then.

**TypeScript with `bun test`.** The scripts are TypeScript already, and
the live client takes the types of requests and answers from
`web/src/lib/wire.ts` (decision 35). `tests/` is its own package with
only the types of Bun and the compiler.

**Three suites, one harness.** `tests/git` covers the Git server,
`tests/around` what surrounds it (accounts, repositories, pull requests,
the live connection), and a later `tests/ui` the pages in a browser.

## Consequences

- The tests need the same programs as local mode, and a build
  (`cargo build`) before they run.
- A change that reaches other servers after a delay, such as a change to
  the catalog, is awaited with `eventually`.
- `sha2` is optimized in development builds. PBKDF2 took three seconds per
  password in an unoptimized build, for every new user and for the first
  request of a user on each server.
- A push cut off by a stopped server is timed against a whole push, so
  which point of the push each run cuts differs. The test accepts both
  outcomes of a push that failed for the client, and requires that every
  server agrees on one.
- Tests wait for what the server does on a timer: up to five seconds for a
  change to the catalog, up to a minute for a snapshot.
- GitHub mirrors and sign-in are not covered: they need a stand-in for
  GitHub.
- Nothing runs the tests automatically yet.
