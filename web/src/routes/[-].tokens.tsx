import { createFileRoute, Link } from "@tanstack/react-router"
import { useState } from "react"
import { useQuery, useQueryClient } from "@tanstack/react-query"
import { Plus, Trash2 } from "lucide-react"

import { useLive } from "@/lib/api"
import { ago, fullDate } from "@/lib/format"
import {
  createToken,
  listTokens,
  removeToken,
  type TokenAccess,
} from "@/lib/tokens"
import { CopyButton } from "@/components/repo/clone"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"

export const Route = createFileRoute("/-/tokens")({ component: Tokens })

interface AccessRow {
  repo: string
  level: TokenAccess
}

function Tokens() {
  const state = useLive()
  const qc = useQueryClient()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [message, setMessage] = useState<string | null>(null)
  const [secret, setSecret] = useState<string | null>(null)
  const [limited, setLimited] = useState(false)
  const [access, setAccess] = useState<AccessRow[]>([
    { repo: "*", level: "read" },
  ])
  const query = useQuery({
    queryKey: ["tokens", state?.user],
    queryFn: listTokens,
    enabled: !!state?.user,
    retry: false,
  })

  async function create(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault()
    const form = e.currentTarget
    const data = new FormData(form)
    setBusy(true)
    setError(null)
    setMessage(null)
    setSecret(null)
    try {
      const scoped = limited
        ? Object.fromEntries(
            access.map(({ repo, level }) => [repo.trim(), level])
          )
        : undefined
      if (limited && access.some(({ repo }) => !repo.trim())) {
        throw new Error("Every access rule needs a repository name or *.")
      }
      const expiry = String(data.get("expires_days") ?? "none")
      const created = await createToken({
        name: String(data.get("name")),
        access: scoped,
        ...(expiry !== "none" ? { expires_days: Number(expiry) } : {}),
      })
      form.reset()
      setLimited(false)
      setAccess([{ repo: "*", level: "read" }])
      setSecret(created.secret)
      setMessage("Token created. Copy it now; it won’t be shown again.")
      await qc.invalidateQueries({ queryKey: ["tokens"] })
    } catch (err) {
      setError(err instanceof Error ? err.message : "Token request failed.")
    } finally {
      setBusy(false)
    }
  }

  async function remove(id: number) {
    setBusy(true)
    setError(null)
    setMessage(null)
    try {
      await removeToken(id)
      setMessage("Token revoked.")
      await qc.invalidateQueries({ queryKey: ["tokens"] })
    } catch (err) {
      setError(err instanceof Error ? err.message : "Token request failed.")
    } finally {
      setBusy(false)
    }
  }

  function updateAccess(index: number, change: Partial<AccessRow>) {
    setAccess((rows) =>
      rows.map((row, i) => (i === index ? { ...row, ...change } : row))
    )
  }

  if (!state?.ready) return <p>Connecting…</p>
  if (!state.user)
    return (
      <p>
        <Link
          to="/-/sign-in"
          search={{ next: "/-/tokens" }}
          className="underline"
        >
          Sign in
        </Link>{" "}
        to manage your access tokens.
      </p>
    )

  return (
    <div className="mx-auto grid max-w-2xl gap-6">
      <Card>
        <CardHeader>
          <CardTitle>Access tokens</CardTitle>
          <CardDescription>
            Create credentials for Git clients, automation, and CI. Tokens can
            never have more access than your account.
          </CardDescription>
        </CardHeader>
        <CardContent className="grid gap-6">
          {(error || query.error) && (
            <Alert variant="destructive">
              <AlertDescription>
                {error ?? query.error?.message}
              </AlertDescription>
            </Alert>
          )}
          {secret && (
            <Alert>
              <AlertTitle>Copy your new token</AlertTitle>
              <AlertDescription className="grid gap-2">
                <p>{message}</p>
                <div className="flex gap-2">
                  <Input
                    readOnly
                    value={secret}
                    className="font-mono"
                    onFocus={(e) => e.currentTarget.select()}
                  />
                  <CopyButton text={secret} />
                </div>
              </AlertDescription>
            </Alert>
          )}
          {!secret && message && (
            <p role="status" className="text-sm">
              {message}
            </p>
          )}

          <form onSubmit={create} className="grid gap-4">
            <div className="grid gap-2">
              <Label htmlFor="token-name">Token name</Label>
              <Input
                id="token-name"
                name="name"
                placeholder="CI for org/app"
                required
                maxLength={100}
                disabled={busy}
              />
            </div>
            <div className="grid gap-2">
              <Label htmlFor="token-expiry">Expiration</Label>
              <Select name="expires_days" defaultValue="none" disabled={busy}>
                <SelectTrigger id="token-expiry" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="none">No expiry</SelectItem>
                  <SelectItem value="7">7 days</SelectItem>
                  <SelectItem value="30">30 days</SelectItem>
                  <SelectItem value="90">90 days</SelectItem>
                  <SelectItem value="180">180 days</SelectItem>
                  <SelectItem value="365">1 year</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <label className="flex items-start gap-2 text-sm">
              <input
                type="checkbox"
                className="mt-0.5 size-4"
                checked={limited}
                disabled={busy}
                onChange={(e) => setLimited(e.target.checked)}
              />
              <span>
                Limit this token’s repository access
                <span className="block text-muted-foreground">
                  Otherwise it uses your current account access.
                </span>
              </span>
            </label>
            {limited && (
              <div className="grid gap-3 rounded-md border p-3">
                <Label>Access rules</Label>
                {access.map((rule, index) => (
                  <div key={index} className="flex gap-2">
                    <Input
                      aria-label={`Repository for rule ${index + 1}`}
                      value={rule.repo}
                      placeholder="org/repository or *"
                      required
                      disabled={busy}
                      onChange={(e) =>
                        updateAccess(index, { repo: e.target.value })
                      }
                    />
                    <Select
                      value={rule.level}
                      disabled={busy}
                      onValueChange={(level) =>
                        updateAccess(index, { level: level as TokenAccess })
                      }
                    >
                      <SelectTrigger className="w-28" aria-label="Access level">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="none">None</SelectItem>
                        <SelectItem value="read">Read</SelectItem>
                        <SelectItem value="write">Write</SelectItem>
                        <SelectItem value="force">Force</SelectItem>
                      </SelectContent>
                    </Select>
                    <Button
                      type="button"
                      size="icon"
                      variant="outline"
                      aria-label={`Remove access rule ${index + 1}`}
                      disabled={busy || access.length === 1}
                      onClick={() =>
                        setAccess((rows) => rows.filter((_, i) => i !== index))
                      }
                    >
                      <Trash2 />
                    </Button>
                  </div>
                ))}
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className="w-fit"
                  disabled={busy}
                  onClick={() =>
                    setAccess((rows) => [...rows, { repo: "", level: "read" }])
                  }
                >
                  <Plus />
                  Add rule
                </Button>
              </div>
            )}
            <Button disabled={busy}>
              {busy ? "Working…" : "Create token"}
            </Button>
          </form>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Your tokens</CardTitle>
          <CardDescription>
            Revoking a token immediately stops clients that use it.
          </CardDescription>
        </CardHeader>
        <CardContent>
          {query.isPending ? (
            <p>Loading tokens…</p>
          ) : query.data?.tokens.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No access tokens yet.
            </p>
          ) : (
            <ul className="grid gap-3">
              {query.data?.tokens.map((token) => (
                <li
                  key={token.id}
                  className="flex items-start justify-between gap-4 rounded-md border p-3"
                >
                  <div className="grid min-w-0 gap-1">
                    <span className="font-medium break-words">
                      {token.name}
                    </span>
                    <div className="flex flex-wrap gap-1">
                      {token.access === null ? (
                        <Badge variant="secondary">Account access</Badge>
                      ) : (
                        Object.entries(token.access).map(([repo, level]) => (
                          <Badge key={repo} variant="outline">
                            {repo}: {level}
                          </Badge>
                        ))
                      )}
                    </div>
                    <span
                      className="text-xs text-muted-foreground"
                      title={fullDate(token.created_ms / 1000)}
                    >
                      Created {ago(token.created_ms / 1000)} ·{" "}
                      {token.expires_ms
                        ? `expires ${ago(token.expires_ms / 1000)}`
                        : "no expiry"}
                      {token.last_used_ms
                        ? ` · used ${ago(token.last_used_ms / 1000)}`
                        : " · never used"}
                    </span>
                  </div>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={busy}
                    onClick={() => remove(token.id)}
                    aria-label={`Revoke ${token.name}`}
                  >
                    Revoke
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
