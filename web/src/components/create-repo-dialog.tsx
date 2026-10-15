import { useNavigate } from "@tanstack/react-router"
import { BookLock, BookOpen, Plus } from "lucide-react"
import { useEffect, useState, type FormEvent } from "react"

import { useCreateRepo, useLive, useRepos } from "@/lib/api"
import { href } from "@/lib/paths"
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
import { Kbd } from "@/components/ui/kbd"
import { Label } from "@/components/ui/label"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"

type Visibility = "private" | "public"

export function CreateRepoDialog({
  open: controlledOpen,
  onOpenChange,
  trigger = true,
}: {
  open?: boolean
  onOpenChange?: (open: boolean) => void
  trigger?: boolean
} = {}) {
  const user = useLive()?.user
  const navigate = useNavigate()
  const create = useCreateRepo()
  const repos = useRepos()
  const [internalOpen, setInternalOpen] = useState(false)
  const [name, setName] = useState("")
  const [checkedName, setCheckedName] = useState("")
  const [visibility, setVisibility] = useState<Visibility>("private")
  const open = controlledOpen ?? internalOpen
  const trimmedName = name.trim()
  const repository = user ? `${user}/${trimmedName}` : ""
  const checking =
    trimmedName !== "" && (checkedName !== trimmedName || repos.isFetching)
  const exists =
    checkedName === trimmedName &&
    repos.data?.repos.some((repo) => repo.name === repository)

  useEffect(() => {
    if (!open || !trimmedName) return
    const timer = setTimeout(() => {
      setCheckedName(trimmedName)
      void repos.refetch()
    }, 400)
    return () => clearTimeout(timer)
  }, [open, trimmedName, repos.refetch])

  if (!user) return null

  const close = (next: boolean) => {
    setInternalOpen(next)
    onOpenChange?.(next)
    if (!next) {
      setName("")
      setCheckedName("")
      setVisibility("private")
      create.reset()
    }
  }

  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!trimmedName || checking || exists || create.isPending) return

    try {
      const result = await create.mutateAsync({
        name: repository,
        public: visibility === "public",
      })
      close(false)
      navigate({ to: "/$", params: { _splat: href(result.repo.name) } })
    } catch {
      // The mutation error is rendered below the fields.
    }
  }

  return (
    <Dialog open={open} onOpenChange={close}>
      {trigger && (
        <DialogTrigger asChild>
          <Button>
            <Plus />
            New repository
          </Button>
        </DialogTrigger>
      )}
      <DialogContent>
        <form className="contents" onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>Create a new repository</DialogTitle>
            <DialogDescription className="sr-only">
              Start with an empty repository under your account.
            </DialogDescription>
          </DialogHeader>

          <div className="grid gap-2">
            <Label htmlFor="repository-name">Repository name</Label>
            <div className="flex items-center gap-2">
              <span className="shrink-0 text-sm text-muted-foreground">
                {user} /
              </span>
              <Input
                id="repository-name"
                name="name"
                autoFocus
                autoComplete="off"
                placeholder="my-project"
                value={name}
                aria-invalid={exists || create.isError}
                onChange={(event) => {
                  setName(event.target.value)
                  if (create.isError) create.reset()
                }}
              />
            </div>
            {trimmedName &&
              (checking ? (
                <p className="text-xs text-muted-foreground">
                  Checking availability…
                </p>
              ) : exists ? (
                <p className="text-xs text-destructive">
                  A repository with this name already exists.
                </p>
              ) : repos.error ? (
                <p className="text-xs text-muted-foreground">
                  Couldn’t check availability.
                </p>
              ) : (
                <p className="text-xs text-emerald-600">
                  Repository name is available.
                </p>
              ))}
          </div>

          <div className="grid gap-2">
            <Label htmlFor="repository-visibility">Visibility</Label>
            <Select
              value={visibility}
              onValueChange={(value) => setVisibility(value as Visibility)}
            >
              <SelectTrigger id="repository-visibility" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent position="popper">
                <SelectItem value="private">
                  <BookLock />
                  Private
                </SelectItem>
                <SelectItem value="public">
                  <BookOpen />
                  Public
                </SelectItem>
              </SelectContent>
            </Select>
            <p className="text-xs text-muted-foreground">
              {visibility === "public"
                ? "Anyone can view and clone this repository."
                : "Only people with access can view this repository."}
            </p>
          </div>

          {create.error && <QueryError error={create.error} />}

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => close(false)}
            >
              Cancel
              <Kbd>Esc</Kbd>
            </Button>
            <Button
              type="submit"
              disabled={!trimmedName || checking || exists || create.isPending}
            >
              {create.isPending ? "Creating…" : "Create repository"}
              {!create.isPending && (
                <Kbd className="bg-primary-foreground/15 text-primary-foreground">
                  Enter
                </Kbd>
              )}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
