import { createFileRoute, Link, useNavigate } from "@tanstack/react-router"
import { useQuery } from "@tanstack/react-query"
import { useEffect, useState } from "react"

import { githubSignInHref, signIn, signUpOptions } from "@/lib/session"
import {
  signInWithPasskey,
  passkeyError,
  supportsPasskeys,
} from "@/lib/passkeys"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import { GithubMark } from "@/components/github-mark"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { Crumbs, PageHeader } from "@/components/frame"
import { Label } from "@/components/ui/label"

export const Route = createFileRoute("/-/sign-in")({
  validateSearch: (
    s: Record<string, unknown>
  ): { next?: string; github_error?: string } => ({
    next:
      typeof s.next === "string" && s.next.startsWith("/") ? s.next : undefined,
    github_error:
      typeof s.github_error === "string" ? s.github_error : undefined,
  }),
  component: SignIn,
})

function SignIn() {
  const { next, github_error } = Route.useSearch()
  const navigate = useNavigate()
  const [error, setError] = useState<string | null>(github_error ?? null)
  const [busy, setBusy] = useState(false)
  const [withPassword, setWithPassword] = useState(false)

  const [passkeySupported, setPasskeySupported] = useState(false)
  useEffect(() => setPasskeySupported(supportsPasskeys()), [])
  const signUp = useQuery({
    queryKey: ["sign-up"],
    queryFn: signUpOptions,
    retry: false,
  })

  function destination() {
    return next &&
      !next.startsWith("//") &&
      !next.includes("\\") &&
      (!next.startsWith("/-/") ||
        next === "/-/settings" ||
        next.startsWith("/-/settings/"))
      ? next
      : "/"
  }

  async function passkey() {
    setBusy(true)
    setError(null)
    try {
      await signInWithPasskey()
      navigate({ href: destination() })
    } catch (err) {
      setError(passkeyError(err))
    } finally {
      setBusy(false)
    }
  }

  async function submit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault()
    const form = new FormData(e.currentTarget)
    setBusy(true)
    setError(null)
    try {
      await signIn(String(form.get("user")), String(form.get("password")))
      navigate({ href: destination() })
    } catch (err) {
      setError(err instanceof Error ? err.message : "sign-in failed")
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      <PageHeader>
        <Crumbs items={[{ label: "Sign in" }]} />
      </PageHeader>
      <div className="flex justify-center px-4 py-16">
        <Card className="w-full max-w-sm">
          <CardHeader>
            <CardTitle>Sign in</CardTitle>
            <CardDescription>
              Use your password or a registered passkey.
            </CardDescription>
          </CardHeader>
          <CardContent>
            {error && (
              <Alert variant="destructive" className="mb-4">
                <AlertDescription>{error}</AlertDescription>
              </Alert>
            )}
            {withPassword || !(passkeySupported || signUp.data?.github) ? (
              <form onSubmit={submit} className="grid gap-4">
                <div className="grid gap-2">
                  <Label htmlFor="user">User name</Label>
                  <Input
                    id="user"
                    name="user"
                    autoComplete="username"
                    required
                    autoFocus={withPassword}
                  />
                </div>
                <div className="grid gap-2">
                  <Label htmlFor="password">Password</Label>
                  <Input
                    id="password"
                    name="password"
                    type="password"
                    autoComplete="current-password"
                    required
                  />
                </div>
                <Button type="submit" disabled={busy}>
                  {busy ? "Signing in…" : "Sign in"}
                </Button>
                {withPassword && (
                  <Button
                    type="button"
                    variant="ghost"
                    disabled={busy}
                    onClick={() => setWithPassword(false)}
                  >
                    Other ways
                  </Button>
                )}
              </form>
            ) : (
              <div className="grid gap-4">
                {passkeySupported && (
                  <Button type="button" disabled={busy} onClick={passkey}>
                    Sign in with a passkey
                  </Button>
                )}
                {signUp.data?.github && (
                  <Button
                    type="button"
                    variant="outline"
                    disabled={busy}
                    asChild
                  >
                    <a href={githubSignInHref(destination())}>
                      <GithubMark />
                      Sign in with GitHub
                    </a>
                  </Button>
                )}
                <Button
                  type="button"
                  variant="outline"
                  disabled={busy}
                  onClick={() => {
                    setError(null)
                    setWithPassword(true)
                  }}
                >
                  Sign in with a password
                </Button>
              </div>
            )}
            {signUp.data?.enabled && (
              <p className="mt-4 text-center text-sm text-muted-foreground">
                No account yet?{" "}
                <Link to="/-/sign-up" className="underline">
                  Sign up
                </Link>
              </p>
            )}
          </CardContent>
        </Card>
      </div>
    </>
  )
}
