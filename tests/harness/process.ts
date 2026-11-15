// Child processes of the tests.

export type Output = { code: number; stdout: string; stderr: string }

export type RunOptions = { cwd?: string; input?: string; env?: Record<string, string> }

/** Runs a command to its end, whatever its exit code. */
export async function attempt(cmd: string[], o: RunOptions = {}): Promise<Output> {
  const p = Bun.spawn(cmd, {
    cwd: o.cwd,
    env: { ...process.env, ...o.env },
    stdin: o.input === undefined ? "ignore" : Buffer.from(o.input),
    stdout: "pipe",
    stderr: "pipe",
  })
  const [stdout, stderr, code] = await Promise.all([
    new Response(p.stdout).text(),
    new Response(p.stderr).text(),
    p.exited,
  ])
  return { code, stdout, stderr }
}

/** Runs a command that must succeed, and answers its output. */
export async function run(cmd: string[], o: RunOptions = {}): Promise<string> {
  const r = await attempt(cmd, o)
  if (r.code !== 0) {
    // Commands can carry a password in a URL.
    const shown = cmd.map((a) => a.replace(/\/\/[^@/]+@/, "//***@")).join(" ")
    throw new Error(`${shown} failed (${r.code}):\n${r.stderr}${r.stdout}`)
  }
  return r.stdout
}

/** Calls `check` until it passes: for changes that reach a server after a delay. */
export async function eventually<T>(check: () => T | Promise<T>, ms = 15_000): Promise<T> {
  const end = Date.now() + ms
  for (;;) {
    try {
      return await check()
    } catch (e) {
      if (Date.now() > end) throw e
      await Bun.sleep(200)
    }
  }
}
