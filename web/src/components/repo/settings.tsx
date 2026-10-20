import { useNavigate } from "@tanstack/react-router"
import { BookLock, BookOpen } from "lucide-react"
import { useState, type FormEvent, type ReactNode } from "react"

import { useRepoSettingsChange, type Refs } from "@/lib/api"
import { href } from "@/lib/paths"
import { cn } from "@/lib/utils"
import { QueryError } from "@/components/query-error"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"

/** The owner's settings of a repository: name, visibility, deletion. */
export function RepoSettings({ repo, refs }: { repo: string; refs: Refs }) {
  const owner = repo.split("/")[0]
  return (
    <div className="grid w-full max-w-2xl gap-8">
      <Rename repo={repo} owner={owner} />
      <Visibility repo={repo} isPublic={refs.public} />
      <Delete repo={repo} />
    </div>
  )
}

function Section({
  title,
  description,
  danger,
  children,
}: {
  title: string
  description: string
  danger?: boolean
  children: ReactNode
}) {
  return (
    <section
      className={cn(
        "grid gap-3 rounded-md border p-4",
        danger && "border-destructive/40"
      )}
    >
      <div className="grid gap-1">
        <h2 className={cn("text-sm font-medium", danger && "text-destructive")}>
          {title}
        </h2>
        <p className="text-sm text-muted-foreground">{description}</p>
      </div>
      {children}
    </section>
  )
}

function Rename({ repo, owner }: { repo: string; owner: string }) {
  const navigate = useNavigate()
  const change = useRepoSettingsChange(repo)
  const current = repo.slice(owner.length + 1)
  const [name, setName] = useState(current)
  const trimmed = name.trim()
  const unchanged = trimmed === current

  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!trimmed || unchanged || change.isPending) return
    try {
      const { repo: renamed } = await change.mutateAsync({
        op: "repo_rename",
        name: `${owner}/${trimmed}`,
      })
      if (renamed)
        navigate({
          to: "/$",
          params: { _splat: href(renamed.name, "settings") },
          replace: true,
        })
    } catch {
      // The mutation error is rendered below the field.
    }
  }

  return (
    <Section
      title="Repository name"
      description="Clone URLs and links with the old name stop working; update your remotes after renaming."
    >
      <form className="grid gap-3" onSubmit={submit}>
        <Label htmlFor="repository-name" className="sr-only">
          Repository name
        </Label>
        <div className="flex items-center gap-2">
          <span className="shrink-0 text-sm text-muted-foreground">
            {owner} /
          </span>
          <Input
            id="repository-name"
            autoComplete="off"
            value={name}
            aria-invalid={change.isError}
            onChange={(event) => {
              setName(event.target.value)
              if (change.isError) change.reset()
            }}
          />
          <Button
            type="submit"
            variant="outline"
            disabled={!trimmed || unchanged || change.isPending}
          >
            {change.isPending ? "Renaming…" : "Rename"}
          </Button>
        </div>
        {change.error && <QueryError error={change.error} />}
      </form>
    </Section>
  )
}

function Visibility({ repo, isPublic }: { repo: string; isPublic: boolean }) {
  const change = useRepoSettingsChange(repo)
  return (
    <Section
      title="Visibility"
      description={
        isPublic
          ? "Public: anyone can view and clone this repository, also without signing in."
          : "Private: only people with access can view this repository."
      }
    >
      <div className="flex items-center gap-3">
        <span className="flex items-center gap-2 text-sm">
          {isPublic ? (
            <BookOpen className="size-4" />
          ) : (
            <BookLock className="size-4" />
          )}
          {isPublic ? "Public" : "Private"}
        </span>
        <Button
          variant="outline"
          className="ml-auto"
          disabled={change.isPending}
          onClick={() =>
            change.mutate({ op: "repo_visibility", public: !isPublic })
          }
        >
          {change.isPending
            ? "Changing…"
            : isPublic
              ? "Make private"
              : "Make public"}
        </Button>
      </div>
      {change.error && <QueryError error={change.error} />}
    </Section>
  )
}

function Delete({ repo }: { repo: string }) {
  const navigate = useNavigate()
  const change = useRepoSettingsChange(repo)
  const [open, setOpen] = useState(false)
  const [confirm, setConfirm] = useState("")

  const close = (next: boolean) => {
    setOpen(next)
    if (!next) {
      setConfirm("")
      change.reset()
    }
  }

  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (confirm !== repo || change.isPending) return
    try {
      await change.mutateAsync({ op: "repo_delete" })
      navigate({ to: "/", replace: true })
    } catch {
      // The mutation error is rendered in the dialog.
    }
  }

  return (
    <Section
      title="Delete this repository"
      description="Its history, branches, tags, and pull requests are gone for everyone. The name can be used again."
      danger
    >
      <Dialog open={open} onOpenChange={close}>
        <DialogTrigger asChild>
          <Button variant="destructive" className="justify-self-start">
            Delete repository
          </Button>
        </DialogTrigger>
        <DialogContent>
          <form className="contents" onSubmit={submit}>
            <DialogHeader>
              <DialogTitle>Delete {repo}?</DialogTitle>
              <DialogDescription>
                This cannot be undone from the web interface.
              </DialogDescription>
            </DialogHeader>
            <div className="grid gap-2">
              <Label htmlFor="confirm-delete">
                Type <span className="font-mono">{repo}</span> to confirm
              </Label>
              <Input
                id="confirm-delete"
                autoFocus
                autoComplete="off"
                value={confirm}
                onChange={(event) => setConfirm(event.target.value)}
              />
            </div>
            {change.error && <QueryError error={change.error} />}
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => close(false)}
              >
                Cancel
              </Button>
              <Button
                type="submit"
                variant="destructive"
                disabled={confirm !== repo || change.isPending}
              >
                {change.isPending ? "Deleting…" : "Delete repository"}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </Section>
  )
}
