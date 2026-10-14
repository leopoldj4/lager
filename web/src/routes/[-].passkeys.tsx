import { createFileRoute, Link } from "@tanstack/react-router"
import { useEffect, useState } from "react"
import { useQuery, useQueryClient } from "@tanstack/react-query"
import { useLive } from "@/lib/api"
import {
  addPasskey,
  listPasskeys,
  passkeyError,
  removePasskey,
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

export const Route = createFileRoute("/-/passkeys")({ component: Passkeys })

function Passkeys() {
  const state = useLive()
  const qc = useQueryClient()
  const [supported, setSupported] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [message, setMessage] = useState<string | null>(null)
  useEffect(() => setSupported(supportsPasskeys()), [])
  const query = useQuery({
    queryKey: ["passkeys", state?.user],
    queryFn: listPasskeys,
    enabled: !!state?.user,
    retry: false,
  })

  async function add(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault()
    const form = e.currentTarget
    const data = new FormData(form)
    setBusy(true)
    setError(null)
    setMessage(null)
    try {
      await addPasskey(String(data.get("name")))
      form.reset()
      setMessage("Passkey added. You can use it the next time you sign in.")
      await qc.invalidateQueries({ queryKey: ["passkeys"] })
    } catch (err) {
      setError(passkeyError(err))
    } finally {
      setBusy(false)
    }
  }

  async function remove(id: string) {
    setBusy(true)
    setError(null)
    setMessage(null)
    try {
      await removePasskey(id)
      setMessage("Passkey removed.")
      await qc.invalidateQueries({ queryKey: ["passkeys"] })
    } catch (err) {
      setError(passkeyError(err))
    } finally {
      setBusy(false)
    }
  }

  if (!state?.ready) return <p>Connecting…</p>
  if (!state.user)
    return (
      <p>
        <Link
          to="/-/sign-in"
          search={{ next: "/-/passkeys" }}
          className="underline"
        >
          Sign in
        </Link>{" "}
        to manage your passkeys.
      </p>
    )

  return (
    <Card className="mx-auto max-w-lg">
      <CardHeader>
        <CardTitle>Passkeys</CardTitle>
        <CardDescription>
          Sign in with your fingerprint, face, device PIN, or security key. Your
          password still works for account recovery.
        </CardDescription>
      </CardHeader>
      <CardContent className="grid gap-6">
        {(error || query.error) && (
          <Alert variant="destructive">
            <AlertDescription>{error ?? query.error?.message}</AlertDescription>
          </Alert>
        )}
        {message && (
          <p role="status" className="text-sm">
            {message}
          </p>
        )}
        {query.isPending ? (
          <p>Loading passkeys…</p>
        ) : (
          query.data && (
            <>
              {query.data.passkeys.length === 0 ? (
                <p className="text-sm text-muted-foreground">
                  No passkeys registered yet.
                </p>
              ) : (
                <ul className="grid gap-2">
                  {query.data.passkeys.map((key) => (
                    <li
                      key={key.id}
                      className="flex items-center justify-between gap-4 rounded-md border p-3"
                    >
                      <span className="min-w-0 break-words">{key.name}</span>
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={busy || !query.data.enabled}
                        onClick={() => remove(key.id)}
                        aria-label={`Remove ${key.name}`}
                      >
                        Remove
                      </Button>
                    </li>
                  ))}
                </ul>
              )}
              {!query.data.enabled ? (
                <p>Passkeys have not been enabled on this server.</p>
              ) : !supported ? (
                <p>
                  Use a browser that supports passkeys over HTTPS (or
                  localhost).
                </p>
              ) : (
                <form onSubmit={add} className="grid gap-4">
                  <div className="grid gap-2">
                    <Label htmlFor="passkey-name">Passkey name</Label>
                    <Input
                      id="passkey-name"
                      name="name"
                      placeholder="Personal laptop"
                      required
                      maxLength={100}
                      disabled={busy}
                    />
                  </div>
                  <Button disabled={busy || query.data.passkeys.length >= 20}>
                    {busy ? "Working…" : "Add a passkey"}
                  </Button>
                  {query.data.passkeys.length >= 20 && (
                    <p className="text-sm">
                      You can register up to 20 passkeys. Remove one to add
                      another.
                    </p>
                  )}
                </form>
              )}
            </>
          )
        )}
      </CardContent>
    </Card>
  )
}
