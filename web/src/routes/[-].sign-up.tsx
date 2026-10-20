import { createFileRoute, Link, useNavigate } from "@tanstack/react-router"
import { useQuery } from "@tanstack/react-query"
import { useEffect, useState } from "react"

import {
  passkeyError,
  signUpEnabled,
  signUpWithPasskey,
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
import { Skeleton } from "@/components/ui/skeleton"

export const Route = createFileRoute("/-/sign-up")({ component: SignUp })

function SignUp() {
  const navigate = useNavigate()
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [supported, setSupported] = useState(false)
  useEffect(() => setSupported(supportsPasskeys()), [])
  const enabled = useQuery({
    queryKey: ["sign-up"],
    queryFn: signUpEnabled,
    retry: false,
  })

  async function submit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault()
    const form = new FormData(e.currentTarget)
    setBusy(true)
    setError(null)
    try {
      await signUpWithPasskey(String(form.get("user")))
      navigate({ to: "/" })
    } catch (err) {
      setError(passkeyError(err))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="flex justify-center py-16">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <CardTitle>Create an account</CardTitle>
          <CardDescription>
            Pick a user name and save a passkey for it. No password needed.
          </CardDescription>
        </CardHeader>
        <CardContent>
          {enabled.isPending ? (
            <Skeleton className="h-24" />
          ) : !enabled.data?.enabled ? (
            <p className="text-sm text-muted-foreground">
              Sign-up is not enabled on this server. Ask an administrator for an
              account.
            </p>
          ) : !supported ? (
            <p className="text-sm text-muted-foreground">
              Use a browser that supports passkeys over HTTPS (or localhost).
            </p>
          ) : (
            <form onSubmit={submit} className="grid gap-4">
              {error && (
                <Alert variant="destructive">
                  <AlertDescription>{error}</AlertDescription>
                </Alert>
              )}
              <div className="grid gap-2">
                <Label htmlFor="user">User name</Label>
                <Input
                  id="user"
                  name="user"
                  autoComplete="username"
                  required
                  maxLength={64}
                  pattern="[A-Za-z0-9_][A-Za-z0-9._\-]*"
                  title="Letters, digits, '.', '_', or '-', not starting with '.' or '-'"
                  disabled={busy}
                />
                <p className="text-xs text-muted-foreground">
                  Your repositories live under this name.
                </p>
              </div>
              <Button type="submit" disabled={busy}>
                {busy ? "Creating…" : "Create account with a passkey"}
              </Button>
            </form>
          )}
          <p className="mt-4 text-center text-sm text-muted-foreground">
            Already have an account?{" "}
            <Link to="/-/sign-in" className="underline">
              Sign in
            </Link>
          </p>
        </CardContent>
      </Card>
    </div>
  )
}
