// Loaded before the test files (bunfig.toml): starts the cluster once and
// stops it after the last test.

import { afterAll } from "bun:test"
import { Cluster } from "./cluster"

const count = Number(process.env.LAGER_TEST_SERVERS ?? 2)
if (!Number.isInteger(count) || count < 1) throw new Error("LAGER_TEST_SERVERS must be a number from 1")

const started = await Cluster.start(count)
globalThis.lagerCluster = started

let stopped = false
const stop = async () => {
  if (stopped) return
  stopped = true
  await started.stop()
}
afterAll(stop)
for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, async () => {
    await stop()
    process.exit(1)
  })
}
