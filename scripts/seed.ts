#!/usr/bin/env bun
// Seeds a server with demo users, repositories, and pull requests, for
// trying out a new deployment or a local server (scripts/local.sh).
//
//   bun scripts/seed.ts                                  # local server
//   bun scripts/seed.ts --url https://git.example.com --ssh lager
//   bun scripts/seed.ts --reset                          # recreate the demo repositories
//
// Users and repositories need the administration commands: by default the
// local build with .local/lager.toml, `--ssh <host>` runs them in the
// Compose deployment on that host (`--compose-dir`, default /opt/lager),
// and `--cli "<command>"` names any other command. Git pushes and the pull
// requests go to `--url` over HTTP(S), as the demo users.
//
// Without `--reset`, repositories that exist already are left alone; the
// users are created, or get the password and access levels again.

import { existsSync, mkdtempSync, rmSync, writeFileSync, mkdirSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import { parseArgs } from "node:util"

const { values: opts } = parseArgs({
  options: {
    url: { type: "string", default: "http://127.0.0.1:8080" },
    ssh: { type: "string" },
    "compose-dir": { type: "string", default: "/opt/lager" },
    cli: { type: "string" },
    user: { type: "string", default: "demo" },
    reviewer: { type: "string", default: "alex" },
    password: { type: "string", default: "demo-password" },
    reset: { type: "boolean", default: false },
    help: { type: "boolean", short: "h", default: false },
  },
})
if (opts.help) {
  console.log(
    "usage: bun scripts/seed.ts [--url URL] [--ssh HOST [--compose-dir DIR] | --cli CMD]\n" +
      "                           [--user demo] [--reviewer alex] [--password P] [--reset]"
  )
  process.exit(0)
}

const base = new URL(opts.url!.replace(/\/+$/, ""))
const owner = opts.user!
const reviewer = opts.reviewer!
const password = opts.password!
const repoRoot = join(dirname(import.meta.path), "..")

const names = {
  website: `${owner}/website`,
  api: `${owner}/api`,
  cli: `${owner}/tools/cli`,
  notes: `${owner}/notes`,
  empty: `${owner}/empty`,
}

// ---------------------------------------------------------------------------
// Processes

function run(cmd: string[], o: { cwd?: string; input?: string; env?: Record<string, string> } = {}) {
  const r = Bun.spawnSync({
    cmd,
    cwd: o.cwd,
    env: { ...process.env, ...o.env },
    stdin: o.input === undefined ? "ignore" : Buffer.from(o.input),
    stdout: "pipe",
    stderr: "pipe",
  })
  if (r.exitCode !== 0) {
    // Commands can carry the password in a URL.
    const shown = cmd.map((a) => a.replace(/\/\/[^@/]+@/, "//***@")).join(" ")
    throw new Error(`${shown} failed (${r.exitCode}):\n${r.stderr.toString()}${r.stdout.toString()}`)
  }
  return r.stdout.toString()
}

const shq = (s: string) => (/^[\w@%+=:,./-]+$/.test(s) ? s : `'${s.replaceAll("'", `'\\''`)}'`)

function adminCommand(): (args: string[]) => string[] {
  if (opts.ssh) {
    const dir = opts["compose-dir"]!
    return (args) => [
      "ssh",
      opts.ssh!,
      `cd ${shq(dir)} && docker compose exec -T lager lager ${args.map(shq).join(" ")}`,
    ]
  }
  if (opts.cli) {
    const prefix = opts.cli.split(/\s+/).filter(Boolean)
    return (args) => [...prefix, ...args]
  }
  const local = join(repoRoot, ".local/lager.toml")
  const target = process.env.CARGO_TARGET_DIR ?? join(repoRoot, "target")
  const bin = join(target, "debug/lager")
  if (existsSync(local) && existsSync(bin)) return (args) => [bin, "-c", local, ...args]
  return (args) => ["lager", ...args]
}
const adminCmd = adminCommand()
const admin = (args: string[], input?: string) => run(adminCmd(args), { input })

// ---------------------------------------------------------------------------
// Local repositories with made-up history

type Files = Record<string, string | Uint8Array | null>

const people: Record<string, { name: string; email: string }> = {
  [owner]: { name: "Dana Demo", email: `${owner}@example.com` },
  [reviewer]: { name: "Alex Reviewer", email: `${reviewer}@example.com` },
}

class Work {
  dir = mkdtempSync(join(tmpdir(), "lager-seed-"))
  /** Commit time in seconds; every commit moves it forward. */
  private clock: number

  constructor(
    public name: string,
    daysAgo: number,
    initialBranch = "main"
  ) {
    this.clock = Math.floor(Date.now() / 1000) - daysAgo * 86400
    this.git("init", "-q", `--initial-branch=${initialBranch}`)
  }

  git(...args: string[]) {
    return run(["git", ...args], {
      cwd: this.dir,
      env: {
        GIT_CONFIG_GLOBAL: "/dev/null",
        GIT_CONFIG_NOSYSTEM: "1",
        GIT_TERMINAL_PROMPT: "0",
      },
    }).trim()
  }

  private at(by: string, hours: number) {
    this.clock += Math.round(hours * 3600)
    const p = people[by] ?? { name: by, email: `${by}@example.com` }
    const date = `${this.clock} +0000`
    return {
      GIT_AUTHOR_NAME: p.name,
      GIT_AUTHOR_EMAIL: p.email,
      GIT_AUTHOR_DATE: date,
      GIT_COMMITTER_NAME: p.name,
      GIT_COMMITTER_EMAIL: p.email,
      GIT_COMMITTER_DATE: date,
    }
  }

  commit(message: string, files: Files, o: { by?: string; hours?: number } = {}) {
    for (const [path, content] of Object.entries(files)) {
      const full = join(this.dir, path)
      if (content === null) {
        this.git("rm", "-q", "--", path)
        continue
      }
      mkdirSync(dirname(full), { recursive: true })
      writeFileSync(full, content)
      this.git("add", "--", path)
    }
    run(["git", "commit", "-q", "--allow-empty", "-m", message], {
      cwd: this.dir,
      env: { GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_NOSYSTEM: "1", ...this.at(o.by ?? owner, o.hours ?? 7) },
    })
    return this.head()
  }

  move(from: string, to: string) {
    mkdirSync(dirname(join(this.dir, to)), { recursive: true })
    this.git("mv", from, to)
  }

  tag(name: string, message?: string) {
    const env = { GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_NOSYSTEM: "1", ...this.at(owner, 0) }
    const args = message ? ["tag", "-a", name, "-m", message] : ["tag", name]
    run(["git", ...args], { cwd: this.dir, env })
  }

  head(rev = "HEAD") {
    return this.git("rev-parse", rev)
  }

  checkout(branch: string, create = false) {
    this.git("checkout", "-q", ...(create ? ["-b"] : []), branch)
  }

  push(as: string, ...refspecs: string[]) {
    const url = new URL(`${base.href.replace(/\/$/, "")}/${this.name}.git`)
    url.username = as
    url.password = password
    this.git("-c", "credential.helper=", "push", "-q", url.href, ...refspecs)
  }

  remove() {
    rmSync(this.dir, { recursive: true, force: true })
  }
}

// A small PNG (RGB, no filter), so the file views see binary content.
function png(size: number, color: (x: number, y: number) => [number, number, number]) {
  const raw = new Uint8Array(size * (size * 3 + 1))
  for (let y = 0; y < size; y++) {
    raw[y * (size * 3 + 1)] = 0
    for (let x = 0; x < size; x++) raw.set(color(x, y), y * (size * 3 + 1) + 1 + x * 3)
  }
  const chunk = (type: string, data: Uint8Array) => {
    const out = new Uint8Array(12 + data.length)
    const view = new DataView(out.buffer)
    view.setUint32(0, data.length)
    out.set(new TextEncoder().encode(type), 4)
    out.set(data, 8)
    view.setUint32(8 + data.length, Bun.hash.crc32(out.subarray(4, 8 + data.length)))
    return out
  }
  const header = new Uint8Array(13)
  const hv = new DataView(header.buffer)
  hv.setUint32(0, size)
  hv.setUint32(4, size)
  header.set([8, 2, 0, 0, 0], 8)
  const parts = [
    new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk("IHDR", header),
    chunk("IDAT", Bun.deflateSync(raw)),
    chunk("IEND", new Uint8Array()),
  ]
  return Buffer.concat(parts)
}

// ---------------------------------------------------------------------------
// The live connection (docs/decisions/0013-live-connection-protocol.md)

class Live {
  private nextId = 1
  private pending = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void }>()

  private constructor(private ws: WebSocket) {
    ws.onmessage = (e) => {
      const m = JSON.parse(e.data as string)
      const p = m.id != null ? this.pending.get(m.id) : undefined
      if (!p) return
      this.pending.delete(m.id)
      if (m.type === "res") p.resolve(m.data)
      else if (m.type === "error") p.reject(new Error(`${m.code}: ${m.message}`))
    }
  }

  static async open(user: string) {
    const r = await fetch(new URL("/-/api/session", base), {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ user, password }),
    })
    if (!r.ok) throw new Error(`signing in as ${user}: ${r.status} ${await r.text()}`)
    const cookie = r.headers
      .getSetCookie()
      .map((c) => c.split(";")[0])
      .find((c) => /^lager_session=./.test(c))
    if (!cookie) throw new Error(`signing in as ${user}: no session cookie`)
    const url = new URL("/-/api/live", base)
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:"
    // @ts-expect-error: Bun accepts headers.
    const ws = new WebSocket(url.href, { headers: { cookie } })
    const welcome = await new Promise<any>((resolve, reject) => {
      ws.onopen = () => ws.send(JSON.stringify({ type: "hello", v: 1 }))
      ws.onmessage = (e) => resolve(JSON.parse(e.data as string))
      ws.onerror = () => reject(new Error(`live connection for ${user} failed`))
    })
    if (welcome.type !== "welcome" || welcome.user !== user)
      throw new Error(`live connection for ${user}: ${JSON.stringify(welcome)}`)
    return new Live(ws)
  }

  req<T = any>(op: string, params: Record<string, unknown>): Promise<T> {
    const id = this.nextId++
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve, reject })
      this.ws.send(JSON.stringify({ type: "req", id, op, ...params }))
    })
  }

  async open_pull(repo: string, source: string, target: string, title: string, body: string) {
    const r = await this.req("pull_create", { repo, source, target, title, body, request: crypto.randomUUID() })
    return r.pull.number as number
  }

  comment(repo: string, number: number, body: string) {
    return this.req("pull_comment", { repo, number, body, request: crypto.randomUUID() })
  }

  close_pull(repo: string, number: number) {
    return this.req("pull_close", { repo, number })
  }

  merge(repo: string, number: number, head: string, squash = false) {
    return this.req("pull_merge", { repo, number, head, squash, request: crypto.randomUUID() })
  }

  close() {
    this.ws.close(1000)
  }
}

// ---------------------------------------------------------------------------
// Content

const websiteReadme = (title: string) => `# Website

${title}: the marketing site of the demo company. Plain HTML and CSS, no
build step.

![logo](assets/logo.png)

## Getting started

\`\`\`bash
npm install
npm run dev     # http://localhost:5173
\`\`\`

## Layout

| Path | Contents |
|---|---|
| \`index.html\` | The landing page |
| \`about.html\` | Team and history |
| \`css/\` | Styles |
| \`src/components/\` | Shared pieces |

## Checklist

- [x] Landing page
- [x] About page
- [ ] Contact form
- [ ] Dark mode

See [CONTRIBUTING](docs/CONTRIBUTING.md) before opening a pull request.
`

const indexHtml = (hero: string, extra = "") => `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>Demo Company</title>
    <link rel="stylesheet" href="css/style.css" />
  </head>
  <body>
    <header>
      <img src="assets/logo.png" alt="" width="32" height="32" />
      <nav><a href="index.html">Home</a> <a href="about.html">About</a></nav>
    </header>
    <main>
      <h1>${hero}</h1>
      <p>We build small, durable tools for teams that ship every day.</p>
      <a class="button" href="about.html">Learn more</a>
    </main>${extra}
    <footer>
      <p>&copy; Demo Company</p>
    </footer>
    <script src="js/main.js"></script>
  </body>
</html>
`

const styleCss = (dark = false) => `:root {
  --fg: #1b1b1f;
  --bg: #fafafa;
  --accent: #5b5bd6;
}
${
  dark
    ? `
@media (prefers-color-scheme: dark) {
  :root {
    --fg: #ededef;
    --bg: #111113;
    --accent: #8e8ef0;
  }
}
`
    : ""
}
body {
  margin: 0 auto;
  max-width: 48rem;
  font-family: system-ui, sans-serif;
  color: var(--fg);
  background: var(--bg);
}

header {
  display: flex;
  gap: 1rem;
  align-items: center;
  padding: 1rem 0;
}

.button {
  display: inline-block;
  padding: 0.5rem 1rem;
  border-radius: 0.375rem;
  color: white;
  background: var(--accent);
  text-decoration: none;
}
`

const logo = (shift: number) =>
  png(64, (x, y) => [(x * 4 + shift) & 255, (y * 4) & 255, 200 - ((x + y) & 127)])

function cargoToml(version: string) {
  return `[package]
name = "demo-api"
version = "${version}"
edition = "2024"

[dependencies]
axum = "0.8"
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
`
}

const apiMain = (routes: string[]) => `use axum::Router;

mod routes;

#[tokio::main]
async fn main() {
    let app = Router::new()
${routes.map((r) => `        .merge(routes::${r}::router())`).join("\n")};
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
`

const route = (name: string, body = "") => `use axum::{Json, Router, routing::get};
use serde::Serialize;

#[derive(Serialize)]
struct ${name[0].toUpperCase() + name.slice(1)} {
    id: u64,
    name: String,
}

pub fn router() -> Router {
    Router::new().route("/${name}", get(list))
}

async fn list() -> Json<Vec<${name[0].toUpperCase() + name.slice(1)}>> {
${body || "    Json(vec![])"}
}
`

function fixtures(n: number) {
  const rows = Array.from({ length: n }, (_, i) => `  { "id": ${i + 1}, "name": "item ${i + 1}", "tags": ["t${i % 7}", "t${i % 11}"] }`)
  return `[\n${rows.join(",\n")}\n]\n`
}

// ---------------------------------------------------------------------------
// The repositories

async function seedWebsite() {
  const repo = names.website
  const w = new Work(repo, 45)
  try {
    w.commit("Initial site", {
      "README.md": websiteReadme("Version one"),
      "index.html": indexHtml("Build things that last"),
      "css/style.css": styleCss(),
      ".gitignore": "node_modules/\ndist/\n",
      LICENSE: "MIT License\n\nCopyright (c) Demo Company\n",
    })
    w.commit("Add the logo", { "assets/logo.png": logo(0) }, { hours: 20 })
    w.commit("Add the about page", {
      "about.html": "<!doctype html>\n<title>About</title>\n<h1>About us</h1>\n<p>Ten people, one office, many plants.</p>\n",
    }, { by: reviewer })
    w.commit("Add a package.json for the dev server", {
      "package.json": JSON.stringify({ name: "website", private: true, scripts: { dev: "vite" }, devDependencies: { vite: "^7.0.0" } }, null, 2) + "\n",
      "js/main.js": "document.querySelector('.button')?.addEventListener('click', () => console.log('hello'))\n",
    })
    w.commit("Add the navigation component", {
      "src/components/nav/Nav.tsx": `export function Nav() {\n  return (\n    <nav>\n      <a href="/">Home</a>\n      <a href="/about">About</a>\n    </nav>\n  )\n}\n`,
      "src/components/nav/README.md": "# Nav\n\nThe site navigation. Keep it to **five** links or fewer.\n",
    }, { by: reviewer, hours: 30 })
    w.commit("Write the contributing guide", {
      "docs/CONTRIBUTING.md": `# Contributing\n\n1. Branch from \`main\`.\n2. Keep pull requests small.\n3. Ask for a review from someone who did not write it.\n\n> Pull requests are merged by the reviewer, not the author.\n`,
    })
    w.tag("v1.0.0", "First public version")

    // fix/hero-copy changes the headline that main changes later: a conflict.
    w.checkout("fix/hero-copy", true)
    w.commit("Shorter headline", { "index.html": indexHtml("Ship faster") })
    w.checkout("main")
    w.commit("New headline from marketing", { "index.html": indexHtml("Make the web calmer") }, { by: reviewer })
    w.commit("Tune button colors", { "css/style.css": styleCss().replace("#5b5bd6", "#4f46e5") })
    w.tag("v1.1.0")

    w.checkout("feature/dark-mode", true)
    w.commit("Add dark mode colors", { "css/style.css": styleCss(true).replace("#5b5bd6", "#4f46e5") }, { by: reviewer })
    w.commit("Update the checklist", { "README.md": websiteReadme("Version one").replace("- [ ] Dark mode", "- [x] Dark mode") }, { by: reviewer })

    w.checkout("main")
    w.checkout("feature/contact", true)
    w.commit("Add the contact page", {
      "contact.html": "<!doctype html>\n<title>Contact</title>\n<h1>Contact</h1>\n<form>\n  <input name=\"email\" type=\"email\" required />\n  <textarea name=\"message\"></textarea>\n  <button>Send</button>\n</form>\n",
    }, { by: reviewer })

    w.checkout("main")
    w.checkout("chore/images", true)
    w.commit("Recompress the logo", { "assets/logo.png": logo(90) })
    w.commit("Drop the unused about image note", { "src/components/nav/README.md": "# Nav\n\nThe site navigation.\n" })

    w.checkout("main")
    w.checkout("experiment/react", true)
    w.commit("Rewrite the site in React", {
      "index.html": null,
      "about.html": null,
      "src/main.tsx": "import { createRoot } from 'react-dom/client'\n\ncreateRoot(document.getElementById('root')!).render(<h1>Hello</h1>)\n",
    }, { by: reviewer })

    w.checkout("main")
    w.checkout("docs/typo", true)
    w.commit("Fix a typo in the README", { "README.md": websiteReadme("Version one").replace("no\nbuild step", "no\nbuild step at all") })

    w.checkout("main")
    w.push(owner, "refs/heads/*:refs/heads/*", "refs/tags/*:refs/tags/*")
    admin(["repo", "set-public", repo, "true"])

    const demo = await Live.open(owner)
    const alex = await Live.open(reviewer)
    try {
      const dark = await alex.open_pull(repo, "feature/dark-mode", "main", "Add dark mode",
        "Uses `prefers-color-scheme`, so there is no toggle yet.\n\nCloses the last item of the checklist.")
      await demo.comment(repo, dark, "Looks good. Could the accent be a bit lighter in dark mode?")
      // A push to the source branch after the pull request was opened.
      w.checkout("feature/dark-mode")
      w.commit("Lighter accent in dark mode", { "css/style.css": styleCss(true).replace("#5b5bd6", "#4f46e5").replace("#8e8ef0", "#a5a5f5") }, { by: reviewer, hours: 2 })
      w.push(owner, "feature/dark-mode")
      await alex.comment(repo, dark, "Done, lighter now.")

      const hero = await demo.open_pull(repo, "fix/hero-copy", "main", "Shorter headline",
        "Marketing changed the headline in the meantime, so this one needs a rebase.")
      await alex.comment(repo, hero, "This conflicts with the new headline on main.")

      const contact = await alex.open_pull(repo, "feature/contact", "main", "Add the contact page", "A plain form for now; it does not send anything yet.")
      await demo.comment(repo, contact, "Fine for a first version.")
      const images = await demo.open_pull(repo, "chore/images", "main", "Recompress images", "Two small commits, squash them.")
      const react = await alex.open_pull(repo, "experiment/react", "main", "Experiment: rewrite in React", "Just to see how it feels.")
      await demo.comment(repo, react, "Let's not, the plain HTML is a feature.")
      const typo = await demo.open_pull(repo, "docs/typo", "main", "Fix a typo in the README", "")

      // Merged by a push: main moves to the source branch.
      w.checkout("main")
      w.git("merge", "-q", "--ff-only", "docs/typo")
      w.push(owner, "main")

      await alex.merge(repo, contact, w.head("feature/contact"))
      await alex.merge(repo, images, w.head("chore/images"), true)
      await alex.close_pull(repo, react)
      console.log(`  pull requests: #${dark} open, #${hero} conflict, #${contact} merged, #${images} squashed, #${react} closed, #${typo} merged by push`)
    } finally {
      demo.close()
      alex.close()
    }
  } finally {
    w.remove()
  }
}

async function seedApi() {
  const repo = names.api
  const w = new Work(repo, 60)
  try {
    const routes = ["health"]
    let changelog = "# Changelog\n\n"
    w.commit("Start the API", {
      "Cargo.toml": cargoToml("0.1.0"),
      "src/main.rs": apiMain(routes),
      "src/routes/mod.rs": "pub mod health;\n",
      "src/routes/health.rs": route("health"),
      "CHANGELOG.md": changelog,
      "README.md": "# demo-api\n\nA small HTTP API.\n\n```bash\ncargo run\ncurl localhost:3000/health\n```\n",
    }, { hours: 0 })
    // A long history, for the paged commit lists.
    for (let i = 1; i <= 120; i++) {
      changelog += `- Change ${i}\n`
      const files: Files = { "CHANGELOG.md": changelog }
      if (i === 20 || i === 55 || i === 90) {
        const r = ["users", "orders", "invoices"][[20, 55, 90].indexOf(i)]
        routes.push(r)
        files[`src/routes/${r}.rs`] = route(r)
        files["src/routes/mod.rs"] = routes.map((x) => `pub mod ${x};\n`).join("")
        files["src/main.rs"] = apiMain(routes)
      }
      if (i === 40) files["Cargo.toml"] = cargoToml("0.2.0")
      if (i === 100) files["Cargo.toml"] = cargoToml("0.3.0")
      if (i === 70) files["data/fixtures.json"] = fixtures(3000)
      const by = i % 3 === 0 ? reviewer : owner
      w.commit(i === 70 ? "Add test fixtures" : `Change ${i}`, files, { by, hours: 11 })
      if (i === 30) {
        w.move("README.md", "docs/README.md")
        w.commit("Move the README into docs", {}, { hours: 1 })
      }
      if (i === 39) w.tag("v0.1.0", "First release")
      if (i === 40) w.git("branch", "release/0.2")
      if (i === 99) w.tag("v0.2.0", "Second release")
      if (i === 110) w.commit("Remove the fixtures again", { "data/fixtures.json": null }, { hours: 1 })
    }
    w.tag("v0.3.0", "Third release")
    w.checkout("feature/pagination", true)
    w.commit("Page the user list", {
      "src/routes/users.rs": route("users", "    // ?page=N, 50 per page\n    Json(vec![])"),
    })
    w.checkout("main")
    w.push(owner, "refs/heads/*:refs/heads/*", "refs/tags/*:refs/tags/*")
    admin(["repo", "set-ref-rule", repo, "refs/heads/main", "--delete", "force"])
    admin(["repo", "set-ref-rule", repo, "refs/heads/release/*", "--update", "force"])

    // The reviewer can read, so they can open a pull request but not merge it.
    const alex = await Live.open(reviewer)
    try {
      const n = await alex.open_pull(repo, "feature/pagination", "main", "Page the user list", "50 per page, `?page=N`.")
      console.log(`  pull request #${n} by ${reviewer} (read access only)`)
    } finally {
      alex.close()
    }
  } finally {
    w.remove()
  }
}

function seedCli() {
  const w = new Work(names.cli, 20, "trunk")
  try {
    w.commit("Add the demo command", {
      "bin/demo": "#!/bin/sh\nset -eu\necho \"hello from the demo tool\"\n",
      "README.md": "# cli\n\nA nested repository whose default branch is `trunk`.\n\nSee [usage](docs/usage.md).\n",
      "docs/usage.md": "# Usage\n\n```text\ndemo [--verbose]\n```\n\n| Flag | Meaning |\n|---|---|\n| `--verbose` | Say more |\n",
    })
    w.git("update-index", "--chmod=+x", "bin/demo")
    w.commit("Mark bin/demo executable", {}, { hours: 1 })
    // The first push sets the default branch, so it carries only trunk.
    w.push(owner, "trunk")
    w.checkout("next", true)
    w.commit("Add --verbose", { "bin/demo": "#!/bin/sh\nset -eu\n[ \"${1:-}\" = --verbose ] && echo \"verbose\"\necho \"hello from the demo tool\"\n" })
    w.push(owner, "next")
  } finally {
    w.remove()
  }
}

function seedNotes() {
  const w = new Work(names.notes, 10)
  try {
    w.commit("Private notes", { "README.md": `# Notes\n\nPrivate: only ${owner} can read this repository.\n` })
    w.push(owner, "main")
  } finally {
    w.remove()
  }
}

// ---------------------------------------------------------------------------

function existingRepos() {
  return new Set(
    admin(["repo", "list"])
      .split("\n")
      .map((l) => l.split("\t"))
      .filter((f) => f[0] && f[2] !== "Deleted")
      .map((f) => f[0])
  )
}

async function main() {
  console.log(`seeding ${base.origin}`)
  const users = new Set(admin(["user", "list"]).split("\n").map((l) => l.split("\t")[0]).filter(Boolean))
  const access = [`${names.website}=write`, `${names.api}=read`, `${names.cli}=read`]
  for (const name of [owner, reviewer]) {
    const levels = name === reviewer ? access : []
    if (users.has(name)) {
      admin(["user", "set-password", name], password + "\n")
      admin(["user", "set-access", name, ...levels])
      console.log(`user ${name}: password and access set again`)
    } else {
      admin(["user", "create", name, ...levels.flatMap((l) => ["--access", l])], password + "\n")
      console.log(`user ${name}: created`)
    }
  }

  let repos = existingRepos()
  if (opts.reset) {
    const seeded = Object.values(names).filter((n) => repos.has(n))
    for (const n of seeded) admin(["repo", "delete", n])
    if (seeded.length) {
      console.log(`deleted ${seeded.join(", ")}; waiting for the servers to notice`)
      // Catalog changes apply within five seconds on every server.
      await Bun.sleep(6000)
    }
    repos = new Set()
  }

  const steps: [string, () => unknown][] = [
    [names.website, seedWebsite],
    [names.api, seedApi],
    [names.cli, seedCli],
    [names.notes, seedNotes],
    [names.empty, () => admin(["repo", "create", names.empty])],
  ]
  for (const [name, seed] of steps) {
    if (repos.has(name)) {
      console.log(`${name}: exists, skipped (--reset recreates it)`)
      continue
    }
    console.log(`${name}: seeding`)
    await seed()
  }

  const host = base.host
  console.log(`
done. Sign in at ${base.origin}/ as ${owner} or ${reviewer} (password: ${password}).

  ${names.website.padEnd(18)} public; tags, branches, a binary file, six pull requests
  ${names.api.padEnd(18)} private (${reviewer}: read); 120+ commits, renames, a large diff, ref rules
  ${names.cli.padEnd(18)} nested name, default branch trunk (${reviewer}: read)
  ${names.notes.padEnd(18)} private, ${owner} only
  ${names.empty.padEnd(18)} no commits

  git clone ${base.protocol}//${owner}:${password}@${host}/${names.website}.git`)
}

await main()
