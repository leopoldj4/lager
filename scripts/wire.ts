#!/usr/bin/env bun
// Writes web/src/lib/wire.ts: the TypeScript types of the operations that
// the web interface sends over the live connection, from the server's own
// types (docs/decisions/0035-generated-wire-types.md). Run it after a change
// to an operation or to a type in its answer.
//
//   scripts/wire.ts           write the file
//   scripts/wire.ts --check   write nothing; fail if the file is out of date

import { $ } from "bun"
import { readFileSync, writeFileSync } from "node:fs"
import { dirname, join } from "node:path"

const repo = dirname(import.meta.dir)
const web = join(repo, "web")
const file = join(web, "src/lib/wire.ts")

const types = await $`cargo run --quiet --package lager-server --example wire`.cwd(repo).text()
const formatted = await $`bunx prettier --stdin-filepath ${file} < ${new Response(types)}`.cwd(web).text()

if (process.argv[2] === "--check") {
  if (readFileSync(file, "utf8") !== formatted) {
    console.error("web/src/lib/wire.ts is out of date; run scripts/wire.ts")
    process.exit(1)
  }
} else {
  writeFileSync(file, formatted)
}
