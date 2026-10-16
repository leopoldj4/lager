import { createFileRoute } from "@tanstack/react-router"
import { useState } from "react"
import { useQuery, useQueryClient } from "@tanstack/react-query"
import { Terminal } from "lucide-react"

import { useLive } from "@/lib/api"
import { ago, fullDate } from "@/lib/format"
import { addSshKey, listSshKeys, removeSshKey } from "@/lib/ssh-keys"
import { sshCloneUrl, useSshUrl } from "@/components/repo/clone"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Skeleton } from "@/components/ui/skeleton"
import { Textarea } from "@/components/ui/textarea"
import { SettingsHeader } from "@/components/settings-header"

export const Route = createFileRoute("/-/settings/ssh-keys")({
  component: SshKeys,
})

function SshKeys() {
  const state = useLive()
  const qc = useQueryClient()
  const ssh = useSshUrl()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [message, setMessage] = useState<string | null>(null)
  const query = useQuery({
    queryKey: ["ssh-keys", state?.user],
    queryFn: listSshKeys,
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
      const name = String(data.get("name") ?? "").trim()
      const { key } = await addSshKey({
        key: String(data.get("key")),
        ...(name ? { name } : {}),
      })
      form.reset()
      setMessage(`Added ${key.name} (${key.fingerprint}).`)
      await qc.invalidateQueries({ queryKey: ["ssh-keys"] })
    } catch (err) {
      setError(err instanceof Error ? err.message : "SSH key request failed.")
    } finally {
      setBusy(false)
    }
  }

  async function remove(id: number) {
    setBusy(true)
    setError(null)
    setMessage(null)
    try {
      await removeSshKey(id)
      setMessage("SSH key deleted.")
      await qc.invalidateQueries({ queryKey: ["ssh-keys"] })
    } catch (err) {
      setError(err instanceof Error ? err.message : "SSH key request failed.")
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="grid gap-6">
      <SettingsHeader
        title="SSH keys"
        description="Public keys for Git over SSH. A key has your account’s access."
      />
      {(error || query.error) && (
        <Alert variant="destructive">
          <AlertDescription>{error ?? query.error?.message}</AlertDescription>
        </Alert>
      )}
      {message && (
        <p role="status" className="text-sm break-words">
          {message}
        </p>
      )}

      <section className="grid gap-2">
        <h3 className="text-sm font-medium">Your keys</h3>
        {query.isPending ? (
          <Skeleton className="h-14" />
        ) : query.data?.keys.length === 0 ? (
          <p className="rounded-md border border-dashed px-3 py-4 text-center text-sm text-muted-foreground">
            No SSH keys yet.
          </p>
        ) : (
          <ul className="divide-y rounded-md border">
            {query.data?.keys.map((key) => (
              <li key={key.id} className="flex items-start gap-3 px-3 py-2">
                <Terminal className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
                <div className="grid min-w-0 gap-1">
                  <span className="text-sm font-medium break-words">
                    {key.name}
                  </span>
                  <span className="font-mono text-xs break-all text-muted-foreground">
                    {key.fingerprint}
                  </span>
                  <span
                    className="text-xs text-muted-foreground"
                    title={fullDate(key.created_ms / 1000)}
                  >
                    {key.algorithm} · added {ago(key.created_ms / 1000)}
                    {key.last_used_ms
                      ? ` · used ${ago(key.last_used_ms / 1000)}`
                      : " · never used"}
                  </span>
                </div>
                <Button
                  variant="ghost"
                  size="sm"
                  className="ml-auto shrink-0"
                  disabled={busy}
                  onClick={() => remove(key.id)}
                  aria-label={`Delete ${key.name}`}
                >
                  Delete
                </Button>
              </li>
            ))}
          </ul>
        )}
        <p className="text-xs text-muted-foreground">
          Deleting a key immediately stops clients that use it.
        </p>
      </section>

      <section className="grid gap-4">
        <h3 className="text-sm font-medium">Add a key</h3>
        <form onSubmit={add} className="grid gap-4">
          <div className="grid gap-2">
            <Label htmlFor="ssh-key">Public key</Label>
            <Textarea
              id="ssh-key"
              name="key"
              placeholder="ssh-ed25519 AAAA… you@laptop"
              required
              rows={4}
              spellCheck={false}
              className="font-mono text-xs break-all"
              disabled={busy}
            />
            <p className="text-xs text-muted-foreground">
              Paste the contents of <code>~/.ssh/id_ed25519.pub</code>. Create
              one with <code>ssh-keygen -t ed25519</code>. Ed25519, ECDSA, and
              RSA keys work.
            </p>
          </div>
          <div className="grid gap-2">
            <Label htmlFor="ssh-key-name">Name</Label>
            <Input
              id="ssh-key-name"
              name="name"
              placeholder="The key’s comment, such as you@laptop"
              maxLength={100}
              disabled={busy}
            />
          </div>
          <Button disabled={busy} className="w-fit">
            {busy ? "Working…" : "Add key"}
          </Button>
        </form>
        {ssh && (
          <p className="text-xs text-muted-foreground">
            Then clone with{" "}
            <code className="break-all">{sshCloneUrl(ssh, "org/repo")}</code>.
          </p>
        )}
      </section>
    </div>
  )
}
