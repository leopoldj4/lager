# worktree implementation plan

Build the server in three stages. Keep one Rust program for local and production use.

The stages change the work order. They do not remove functions from the design.

## Stage 1. Run locally

**Build**

1. Create one Cargo project with the modules in the design.
2. Select fixed versions of Git and the Rust libraries.
3. Connect the storage module to a local S3-compatible service.
4. Confirm conditional creation, CAS, current reads, and lost-response handling.
5. Examine Git's `proc-receive` hook before further server work.
6. Confirm that Rust controls ref changes and success responses.
7. If the hook cannot provide this control, implement the receive protocol in Rust.
8. Add the repository head, packs, log entries, and recovery.
9. Add HTTP access with development credentials and configured repository names.
10. Support clone, fetch, push, branches, tags, the default branch, and permitted force pushes.
11. Support atomic changes to several refs.
12. Add limited buffers, disk checks, and process cleanup.
13. Provide a local start command.

Keep all accepted stored data during this stage. Use separate directories for storage data and caches. Use a fixed version of the S3 service in the local setup.

**Complete this stage when these checks pass:**

- Two standard Git clients can exchange changes through the server.
- No push success response occurs before the storage service accepts the head change.
- Thin packs, loose objects, annotated tags, and deletion-only pushes work.
- Updates that need no new objects work.
- An invalid ref change rejects the complete push.
- Recovery restores the same refs and objects after complete cache removal.
- A lost CAS response does not cause duplicate changes during internal retries.
- Large transfers stay within the configured memory limit.
- All client access requires the controller's permission and state checks.

## Stage 2. Run on several servers

**Build**

1. Run three server processes with separate caches and shared storage.
2. Confirm the storage requirements with an intended hosted S3 provider.
3. Check the storage head before each Git read request.
4. Add recovery for missing caches and unknown request results.
5. Send requests to different servers.
6. Add preferred server selection after requests work on every server.
7. Prevent duplicate cache reconstruction on each server.
8. Record request order and results for automatic checks.

Require documented storage guarantees from each provider. A local service cannot show another provider's behavior.

**Complete this stage when these checks pass:**

- A read on server B includes a push that server A already accepted.
- Competing changes to the same old ref cannot both succeed.
- Changes to different refs can succeed without lost changes.
- Readers never observe part of a transaction.
- A server with an empty cache can serve a repository after recovery.
- Server loss, delayed requests, and lost responses do not lose accepted changes.
- Loss of all caches still permits recovery from storage.
- A server without storage access rejects new unchecked reads and writes.

## Stage 3. Prepare for production

**Build**

1. Add snapshots and shared pack compaction.
2. Add separate ref views so long reads do not block new local writes.
3. Add cache removal and disk reservations.
4. Add production identity checks, ref permissions, and the repository catalog.
5. Add customer limits, TLS configuration, diagnostic records, and measurements.
6. Add backup and recovery commands.
7. Define rules for safe removal of obsolete stored data.
8. Protect active operations and unknown request results before data removal.
9. Run data removal in report-only mode first.
10. Measure small, medium, and large repositories under combined read and write loads.
11. Confirm the provider guarantees again before each release.

**Complete this stage when these checks pass:**

- Snapshots restore the same state as the complete log.
- Compaction cannot remove a push that another server accepts.
- Old read views remain usable until their readers finish.
- Data removal cannot break active requests or recovery within the retention period.
- Recovery works in an empty independent system.
- Limits prevent disk exhaustion and uncontrolled process or memory use.
- Access checks prevent another customer's data from becoming visible.
- Measurements meet the agreed capacity, delay, and recovery requirements.

## Later extensions

Keep Git LFS, partial clone, user hooks, SHA-256, and the management API as later work.

Keep transactions across repositories as later work. Pull requests from forks and line comments follow [decision 19](docs/decisions/0019-pull-requests.md). User hooks need process isolation. Partial clone needs object-access rules.

Consider push groups and early change notifications when measurements show a benefit. Writes across regions need storage guarantees that preserve the same transaction rules.
