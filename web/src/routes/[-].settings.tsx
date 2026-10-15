import { createFileRoute, Link } from "@tanstack/react-router"
import { useState } from "react"
import { AlertCircle, Lock, RefreshCw, Search } from "lucide-react"

import {
  useGithub,
  useGithubChange,
  useGithubRepos,
  useLive,
  type GithubMirror,
} from "@/lib/api"
import { ago } from "@/lib/format"
import { href } from "@/lib/paths"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import { GithubMark } from "@/components/github-mark"

export const Route = createFileRoute("/-/settings")({
  validateSearch: (s: Record<string, unknown>): { github_error?: string } => ({
    github_error:
      typeof s.github_error === "string" ? s.github_error : undefined,
  }),
  component: Settings,
})

/** Repositories shown in the picker at once. */
const SHOWN = 50

function Settings() {
  const state = useLive()
  const { github_error } = Route.useSearch()
  if (!state?.ready) return <p>Connecting…</p>
  if (!state.user)
    return (
      <p>
        <Link
          to="/-/sign-in"
          search={{ next: "/-/settings" }}
          className="underline"
        >
          Sign in
        </Link>{" "}
        to change your settings.
      </p>
    )
  return (
    <div className="mx-auto grid max-w-2xl gap-6">
      <h1 className="text-xl font-semibold">Settings</h1>
      <Github error={github_error} />
      <p className="text-sm text-muted-foreground">
        Sign-in with passkeys is on the{" "}
        <Link to="/-/passkeys" className="underline">
          passkeys page
        </Link>
        .
      </p>
    </div>
  )
}

function Github({ error }: { error?: string }) {
  const status = useGithub()
  const change = useGithubChange()
  const data = status.data
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <GithubMark className="size-5" />
          GitHub
        </CardTitle>
      </CardHeader>
      <CardContent className="grid gap-6">
        {error && (
          <Alert variant="destructive">
            <AlertCircle />
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}
        {change.error && (
          <Alert variant="destructive">
            <AlertCircle />
            <AlertDescription>{change.error.message}</AlertDescription>
          </Alert>
        )}
        {status.error ? (
          <QueryError error={status.error} />
        ) : !data ? (
          <Skeleton className="h-10" />
        ) : !data.configured ? (
          <p className="text-sm text-muted-foreground">
            GitHub is not set up on this server. An administrator adds a GitHub
            OAuth app to the server configuration under <code>[github]</code>.
          </p>
        ) : !data.account ? (
          <div>
            <Button asChild>
              <a href="/-/api/github/connect">
                <GithubMark className="size-4" />
                Connect with GitHub
              </a>
            </Button>
            {data.mirrors.length > 0 && (
              <p className="mt-3 text-sm text-muted-foreground">
                Your synced repositories wait until you connect again.
              </p>
            )}
          </div>
        ) : (
          <div className="flex flex-wrap items-center gap-3">
            <span className="text-sm">
              Connected as{" "}
              <span className="font-medium">@{data.account.login}</span>
            </span>
            <Button
              variant="outline"
              size="sm"
              className="ml-auto"
              disabled={change.isPending}
              onClick={() => change.mutate({ op: "github_disconnect" })}
            >
              Disconnect
            </Button>
          </div>
        )}
        {data && data.mirrors.length > 0 && <Mirrors mirrors={data.mirrors} />}
        {data?.account && (
          <Picker
            synced={new Map(data.mirrors.map((m) => [m.github, m.name]))}
          />
        )}
      </CardContent>
    </Card>
  )
}

function Mirrors({ mirrors }: { mirrors: GithubMirror[] }) {
  const change = useGithubChange()
  return (
    <section className="grid gap-2">
      <h2 className="text-sm font-medium">Synced repositories</h2>
      <ul className="divide-y rounded-md border">
        {mirrors.map((m) => (
          <li key={m.name} className="flex items-center gap-3 px-3 py-2">
            <div className="grid min-w-0 gap-0.5">
              <Link
                to="/$"
                params={{ _splat: href(m.name) }}
                className="truncate font-medium hover:underline"
              >
                {m.name}
              </Link>
              <span className="truncate text-xs text-muted-foreground">
                from github.com/{m.github} ·{" "}
                {m.error ? (
                  <span className="text-destructive">{m.error}</span>
                ) : m.synced_ms ? (
                  `synced ${ago(m.synced_ms / 1000)}`
                ) : (
                  <span className="inline-flex items-center gap-1">
                    <RefreshCw className="size-3 animate-spin" />
                    first sync running
                  </span>
                )}
              </span>
            </div>
            <Button
              variant="ghost"
              size="sm"
              className="ml-auto shrink-0"
              disabled={change.isPending}
              title="Keep the repository here, stop following GitHub, and accept pushes"
              onClick={() =>
                change.mutate({ op: "github_unmirror", name: m.name })
              }
            >
              Stop syncing
            </Button>
          </li>
        ))}
      </ul>
    </section>
  )
}

function Picker({ synced }: { synced: Map<string, string> }) {
  const [filter, setFilter] = useState("")
  const repos = useGithubRepos(true)
  const change = useGithubChange()
  const pending = change.isPending
    ? (change.variables as { github?: string }).github
    : undefined
  const shown = repos.data?.repos.filter((r) =>
    r.full_name.toLowerCase().includes(filter.toLowerCase())
  )
  return (
    <section className="grid gap-2">
      <h2 className="text-sm font-medium">Add a repository</h2>
      <div className="relative">
        <Search className="absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
        <Input
          placeholder="Find a GitHub repository"
          className="pl-8"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
      </div>
      {repos.error ? (
        <QueryError error={repos.error} />
      ) : !shown ? (
        <div className="grid gap-2">
          {[0, 1, 2].map((i) => (
            <Skeleton key={i} className="h-10" />
          ))}
        </div>
      ) : shown.length === 0 ? (
        <p className="py-4 text-center text-sm text-muted-foreground">
          No repository matches.
        </p>
      ) : (
        <ul className="max-h-96 divide-y overflow-y-auto rounded-md border">
          {shown.slice(0, SHOWN).map((r) => {
            const name = synced.get(r.full_name)
            return (
              <li
                key={r.full_name}
                className="flex items-center gap-3 px-3 py-2"
              >
                <div className="grid min-w-0 gap-0.5">
                  <span className="flex items-center gap-2 truncate text-sm font-medium">
                    {r.full_name}
                    {r.private && (
                      <Badge variant="outline">
                        <Lock />
                        private
                      </Badge>
                    )}
                  </span>
                  {r.description && (
                    <span className="truncate text-xs text-muted-foreground">
                      {r.description}
                    </span>
                  )}
                </div>
                {name ? (
                  <Button variant="ghost" size="sm" className="ml-auto" asChild>
                    <Link to="/$" params={{ _splat: href(name) }}>
                      Synced
                    </Link>
                  </Button>
                ) : (
                  <Button
                    variant="outline"
                    size="sm"
                    className="ml-auto shrink-0"
                    disabled={change.isPending}
                    onClick={() =>
                      change.mutate({
                        op: "github_mirror",
                        github: r.full_name,
                      })
                    }
                  >
                    {pending === r.full_name ? "Connecting…" : "Sync"}
                  </Button>
                )}
              </li>
            )
          })}
          {shown.length > SHOWN && (
            <li className="px-3 py-2 text-xs text-muted-foreground">
              {shown.length - SHOWN} more; type to narrow the list.
            </li>
          )}
        </ul>
      )}
    </section>
  )
}
