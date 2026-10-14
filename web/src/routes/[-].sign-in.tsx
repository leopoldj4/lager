import { createFileRoute, useNavigate } from "@tanstack/react-router"
import { useEffect, useState } from "react"

import { signIn } from "@/lib/api"
import {
  signInWithPasskey,
  passkeyError,
  supportsPasskeys,
} from "@/lib/passkeys"
import { Alert, AlertDescription } from "@/components/ui/alert"
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

export const Route = createFileRoute("/-/sign-in")({
  validateSearch: (s: Record<string, unknown>): { next?: string } => ({
    next:
      typeof s.next === "string" && s.next.startsWith("/") ? s.next : undefined,
  }),
  component: SignIn,
})

function SignIn() {
  const { next } = Route.useSearch()
  const navigate = useNavigate()
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const [passkeySupported, setPasskeySupported] = useState(false)
  useEffect(() => setPasskeySupported(supportsPasskeys()), [])

  function destination() {
    return next &&
      !next.startsWith("//") &&
      !next.includes("\\") &&
      (!next.startsWith("/-/") || next === "/-/passkeys")
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
    <div className="flex justify-center py-16">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <CardTitle>Sign in</CardTitle>
          <CardDescription>
            Use your password or a registered passkey.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={submit} className="grid gap-4">
            {error && (
              <Alert variant="destructive">
                <AlertDescription>{error}</AlertDescription>
              </Alert>
            )}
            {passkeySupported && (
              <Button
                type="button"
                variant="default"
                disabled={busy}
                onClick={passkey}
              >
                Sign in with a passkey
              </Button>
            )}
            {passkeySupported && (
              <p className="text-center text-sm text-muted-foreground">
                Or sign in with your password
              </p>
            )}
            <div className="grid gap-2">
              <Label htmlFor="user">User name</Label>
              <Input id="user" name="user" autoComplete="username" required />
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
            <Button type="submit" variant="outline" disabled={busy}>
              {busy ? "Signing in…" : "Sign in"}
            </Button>
          </form>
        </CardContent>
      </Card>
    </div>
  )
}
