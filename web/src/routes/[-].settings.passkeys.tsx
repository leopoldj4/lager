import { createFileRoute } from "@tanstack/react-router"
import { useEffect, useState } from "react"
import { useQuery, useQueryClient } from "@tanstack/react-query"
import { KeyRound } from "lucide-react"

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
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Skeleton } from "@/components/ui/skeleton"
import { SettingsHeader } from "@/components/settings-header"

export const Route = createFileRoute("/-/settings/passkeys")({
  component: Passkeys,
})

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

  return (
    <div className="grid gap-6">
      <SettingsHeader
        title="Passkeys"
        description={
          query.data && !query.data.password
            ? "Sign in with your fingerprint, face, device PIN, or security key. Your account has no password, so keep at least one passkey; add a second one as a backup."
            : "Sign in with your fingerprint, face, device PIN, or security key. Your password still works for account recovery."
        }
      />
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
        <Skeleton className="h-14" />
      ) : (
        query.data && (
          <>
            <section className="grid gap-2">
              <h3 className="text-sm font-medium">Your passkeys</h3>
              {query.data.passkeys.length === 0 ? (
                <p className="rounded-md border border-dashed px-3 py-4 text-center text-sm text-muted-foreground">
                  No passkeys registered yet.
                </p>
              ) : (
                <ul className="divide-y rounded-md border">
                  {query.data.passkeys.map((key) => (
                    <li
                      key={key.id}
                      className="flex items-center gap-3 px-3 py-2"
                    >
                      <KeyRound className="size-4 shrink-0 text-muted-foreground" />
                      <span className="min-w-0 text-sm font-medium break-words">
                        {key.name}
                      </span>
                      <Button
                        variant="ghost"
                        size="sm"
                        className="ml-auto shrink-0"
                        disabled={
                          busy ||
                          !query.data.enabled ||
                          (!query.data.password &&
                            query.data.passkeys.length === 1)
                        }
                        onClick={() => remove(key.id)}
                        aria-label={`Remove ${key.name}`}
                      >
                        Remove
                      </Button>
                    </li>
                  ))}
                </ul>
              )}
            </section>
            <section className="grid gap-2">
              <h3 className="text-sm font-medium">Add a passkey</h3>
              {!query.data.enabled ? (
                <p className="text-sm text-muted-foreground">
                  Passkeys have not been enabled on this server.
                </p>
              ) : !supported ? (
                <p className="text-sm text-muted-foreground">
                  Use a browser that supports passkeys over HTTPS (or
                  localhost).
                </p>
              ) : query.data.passkeys.length >= 20 ? (
                <p className="text-sm text-muted-foreground">
                  You can register up to 20 passkeys. Remove one to add another.
                </p>
              ) : (
                <form onSubmit={add} className="grid gap-2">
                  <Label htmlFor="passkey-name" className="sr-only">
                    Passkey name
                  </Label>
                  <div className="flex gap-2">
                    <Input
                      id="passkey-name"
                      name="name"
                      placeholder="Name it, like Personal laptop"
                      required
                      maxLength={100}
                      disabled={busy}
                    />
                    <Button disabled={busy} className="shrink-0">
                      {busy ? "Working…" : "Add passkey"}
                    </Button>
                  </div>
                </form>
              )}
            </section>
          </>
        )
      )}
    </div>
  )
}
