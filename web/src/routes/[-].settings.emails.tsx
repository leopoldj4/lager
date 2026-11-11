import { createFileRoute } from "@tanstack/react-router"
import { useState } from "react"
import { useQuery, useQueryClient } from "@tanstack/react-query"
import { Mail } from "lucide-react"

import { useGithub, useLive } from "@/lib/api"
import { addEmail, listEmails, removeEmail } from "@/lib/emails"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Skeleton } from "@/components/ui/skeleton"
import { Badge } from "@/components/ui/badge"
import { GithubMark } from "@/components/github-mark"
import { SettingsHeader } from "@/components/settings-header"

export const Route = createFileRoute("/-/settings/emails")({
  component: Emails,
})

function Emails() {
  const state = useLive()
  const qc = useQueryClient()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const query = useQuery({
    queryKey: ["emails", state?.user],
    queryFn: listEmails,
    enabled: !!state?.user,
    retry: false,
  })
  const github = useGithub()
  // A connection from before `user:email` brings only GitHub's noreply
  // addresses.
  const reconnect =
    !!github.data?.account &&
    !!query.data &&
    !query.data.emails.some(
      (e) => e.github && !e.email.endsWith("@users.noreply.github.com")
    )

  async function run(work: () => Promise<unknown>) {
    setBusy(true)
    setError(null)
    try {
      await work()
      await qc.invalidateQueries({ queryKey: ["emails"] })
      await qc.invalidateQueries({ queryKey: ["profile"] })
    } catch (err) {
      setError(err instanceof Error ? err.message : "Email request failed.")
    } finally {
      setBusy(false)
    }
  }

  function add(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault()
    const form = e.currentTarget
    const email = String(new FormData(form).get("email") ?? "")
    run(async () => {
      await addEmail(email)
      form.reset()
    })
  }

  return (
    <div className="grid gap-6">
      <SettingsHeader
        title="Emails"
        description="Commits with these author emails count on your profile."
      />
      {(error || query.error) && (
        <Alert variant="destructive">
          <AlertDescription>{error ?? query.error?.message}</AlertDescription>
        </Alert>
      )}

      <section className="grid gap-2">
        {query.isPending ? (
          <Skeleton className="h-10" />
        ) : query.data?.emails.length === 0 ? (
          <p className="rounded-md border border-dashed px-3 py-4 text-center text-sm text-muted-foreground">
            No emails yet.
          </p>
        ) : (
          <ul className="divide-y rounded-md border">
            {query.data?.emails.map(({ email, github }) => (
              <li key={email} className="flex h-10 items-center gap-3 px-3">
                <Mail className="size-4 shrink-0 text-muted-foreground" />
                <span className="min-w-0 truncate text-sm">{email}</span>
                {github ? (
                  <Badge
                    variant="outline"
                    className="ml-auto shrink-0"
                    title="Verified by GitHub"
                  >
                    <GithubMark />
                    verified
                  </Badge>
                ) : (
                  <Button
                    variant="ghost"
                    size="sm"
                    className="ml-auto shrink-0"
                    disabled={busy}
                    onClick={() => run(() => removeEmail(email))}
                    aria-label={`Delete ${email}`}
                  >
                    Delete
                  </Button>
                )}
              </li>
            ))}
          </ul>
        )}
        {reconnect && (
          <p className="text-xs text-muted-foreground">
            <a href="/-/api/github/connect" className="underline">
              Reconnect GitHub
            </a>{" "}
            to add its verified emails.
          </p>
        )}
      </section>

      <form onSubmit={add} className="flex gap-2">
        <Input
          name="email"
          type="email"
          placeholder="you@example.com"
          aria-label="Email"
          required
          maxLength={254}
          disabled={busy}
        />
        <Button disabled={busy} className="shrink-0">
          Add
        </Button>
      </form>
    </div>
  )
}
