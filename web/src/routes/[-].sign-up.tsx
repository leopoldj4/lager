import { createFileRoute, Link, useNavigate } from "@tanstack/react-router"
import { useQuery } from "@tanstack/react-query"
import { useEffect, useState } from "react"

import { signUpOptions, signUpWithPassword } from "@/lib/api"
import {
  passkeyError,
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

/** As MIN_PASSWORD_CHARS on the server. */
const MIN_PASSWORD_CHARS = 12

function SignUp() {
  const navigate = useNavigate()
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [passkeySupported, setPasskeySupported] = useState(false)
  useEffect(() => setPasskeySupported(supportsPasskeys()), [])
  const options = useQuery({
    queryKey: ["sign-up"],
    queryFn: signUpOptions,
    retry: false,
  })
  const passkey = options.data?.passkey && passkeySupported

  async function run(
    create: () => Promise<void>,
    message: (e: unknown) => string
  ) {
    setBusy(true)
    setError(null)
    try {
      await create()
      navigate({ to: "/" })
    } catch (err) {
      setError(message(err))
    } finally {
      setBusy(false)
    }
  }

  function submit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault()
    const form = new FormData(e.currentTarget)
    const user = String(form.get("user"))
    const password = String(form.get("password"))
    if (password !== String(form.get("repeat"))) {
      setError("The passwords differ.")
      return
    }
    run(
      () => signUpWithPassword(user, password),
      (err) => (err instanceof Error ? err.message : "sign-up failed")
    )
  }

  function withPasskey(e: React.MouseEvent<HTMLButtonElement>) {
    const form = e.currentTarget.form
    const user = form?.elements.namedItem("user") as HTMLInputElement | null
    if (!user?.reportValidity()) return
    run(() => signUpWithPasskey(user.value), passkeyError)
  }

  return (
    <div className="flex justify-center py-16">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <CardTitle>Create an account</CardTitle>
          <CardDescription>
            Pick a user name and a password
            {passkey ? ", or save a passkey instead" : ""}.
          </CardDescription>
        </CardHeader>
        <CardContent>
          {options.isPending ? (
            <Skeleton className="h-24" />
          ) : !options.data?.enabled ? (
            <p className="text-sm text-muted-foreground">
              Sign-up is not enabled on this server. Ask an administrator for an
              account.
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
              <div className="grid gap-2">
                <Label htmlFor="password">Password</Label>
                <Input
                  id="password"
                  name="password"
                  type="password"
                  autoComplete="new-password"
                  required
                  minLength={MIN_PASSWORD_CHARS}
                  disabled={busy}
                />
                <p className="text-xs text-muted-foreground">
                  At least {MIN_PASSWORD_CHARS} characters.
                </p>
              </div>
              <div className="grid gap-2">
                <Label htmlFor="repeat">Repeat the password</Label>
                <Input
                  id="repeat"
                  name="repeat"
                  type="password"
                  autoComplete="new-password"
                  required
                  minLength={MIN_PASSWORD_CHARS}
                  disabled={busy}
                />
              </div>
              <Button type="submit" disabled={busy}>
                {busy ? "Creating…" : "Create account"}
              </Button>
              {passkey && (
                <>
                  <p className="text-center text-sm text-muted-foreground">
                    Or, without a password
                  </p>
                  <Button
                    type="button"
                    variant="outline"
                    disabled={busy}
                    onClick={withPasskey}
                  >
                    Create account with a passkey
                  </Button>
                </>
              )}
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
