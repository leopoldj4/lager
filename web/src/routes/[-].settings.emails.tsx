import { createFileRoute, useNavigate } from "@tanstack/react-router"
import { useEffect, useRef, useState } from "react"
import { useQuery, useQueryClient } from "@tanstack/react-query"
import { BadgeCheck, Mail } from "lucide-react"

import { useGithub } from "@/lib/github"
import { useLive } from "@/lib/query"
import {
  addEmail,
  listEmails,
  removeEmail,
  sendVerification,
  verifyEmail,
} from "@/lib/emails"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Skeleton } from "@/components/ui/skeleton"
import { Badge } from "@/components/ui/badge"
import { GithubMark } from "@/components/github-mark"
import { SettingsHeader } from "@/components/settings-header"

export const Route = createFileRoute("/-/settings/emails")({
  // The link in a verification mail.
  validateSearch: (s: Record<string, unknown>): { verify?: string } => ({
    verify: typeof s.verify === "string" ? s.verify : undefined,
  }),
  component: Emails,
})

function Emails() {
  const state = useLive()
  const qc = useQueryClient()
  const navigate = useNavigate()
  const { verify } = Route.useSearch()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
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
      (e) =>
        e.verified === "github" &&
        !e.email.endsWith("@users.noreply.github.com")
    )

  async function run(work: () => Promise<unknown>) {
    setBusy(true)
    setError(null)
    setNotice(null)
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

  // Each link is used once, also under React's double effects.
  const used = useRef<string | null>(null)
  useEffect(() => {
    if (!verify || !state?.user || used.current === verify) return
    used.current = verify
    navigate({ to: "/-/settings/emails", search: {}, replace: true })
    run(async () => {
      const { email } = await verifyEmail(verify)
      setNotice(`Verified ${email}.`)
    })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [verify, state?.user])

  function add(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault()
    const form = e.currentTarget
    const email = String(new FormData(form).get("email") ?? "")
    run(async () => {
      const { sent } = await addEmail(email)
      form.reset()
      if (sent) setNotice(`Sent a link to ${email}.`)
    })
  }

  return (
    <div className="grid gap-6">
      <SettingsHeader
        title="Emails"
        description="Commits by verified emails count on your profile."
      />
      {(error || query.error) && (
        <Alert variant="destructive">
          <AlertDescription>{error ?? query.error?.message}</AlertDescription>
        </Alert>
      )}
      {notice && (
        <Alert>
          <AlertDescription>{notice}</AlertDescription>
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
            {query.data?.emails.map(({ email, verified }) => (
              <li key={email} className="flex h-10 items-center gap-3 px-3">
                <Mail className="size-4 shrink-0 text-muted-foreground" />
                <span className="min-w-0 truncate text-sm">{email}</span>
                {verified === "github" ? (
                  <Badge
                    variant="outline"
                    className="ml-auto shrink-0"
                    title="Verified by GitHub"
                  >
                    <GithubMark />
                    verified
                  </Badge>
                ) : (
                  <>
                    {verified === "mail" ? (
                      <Badge
                        variant="outline"
                        className="ml-auto shrink-0"
                        title="Verified by mail"
                      >
                        <BadgeCheck />
                        verified
                      </Badge>
                    ) : (
                      <>
                        <Badge
                          variant="outline"
                          className="ml-auto shrink-0 text-muted-foreground"
                          title="Counts nowhere until verified"
                        >
                          unverified
                        </Badge>
                        {query.data?.mail && (
                          <Button
                            variant="ghost"
                            size="sm"
                            className="shrink-0"
                            disabled={busy}
                            onClick={() =>
                              run(async () => {
                                await sendVerification(email)
                                setNotice(`Sent a link to ${email}.`)
                              })
                            }
                            aria-label={`Resend the link to ${email}`}
                          >
                            Resend
                          </Button>
                        )}
                      </>
                    )}
                    <Button
                      variant="ghost"
                      size="sm"
                      className="shrink-0"
                      disabled={busy}
                      onClick={() => run(() => removeEmail(email))}
                      aria-label={`Delete ${email}`}
                    >
                      Delete
                    </Button>
                  </>
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

      {query.data && !query.data.mail ? (
        <p className="text-sm text-muted-foreground">
          This server sends no mail. Connect GitHub to add its verified emails.
        </p>
      ) : (
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
      )}
    </div>
  )
}
