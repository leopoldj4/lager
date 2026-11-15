import { Link, useNavigate } from "@tanstack/react-router"
import { ArrowLeft, GitBranch } from "lucide-react"
import { useState } from "react"

import { requestId, useCompare, usePullChange } from "@/lib/pulls"
import { useLive } from "@/lib/query"
import type { Commit, Refs } from "@/lib/wire"
import { ago, short } from "@/lib/format"
import { href, splitCompare } from "@/lib/paths"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import { Avatar } from "./avatar"
import { AuthorName } from "./author-name"
import { ChangedFiles } from "./changes"
import { PageTitle, SectionHead, rowList } from "@/components/frame"
import { MessageEditor } from "./message-editor"

/**
 * Compares two branches and opens a pull request that merges `head` into
 * `base`.
 */
export function ComparePage({
  repo,
  rest,
  refs,
}: {
  repo: string
  rest: string[]
  refs: Refs
}) {
  const navigate = useNavigate()
  const { base, head } = splitCompare(rest)
  const names = new Set(refs.branches.map((b) => b.name))
  const valid = names.has(base) && names.has(head) && base !== head
  const q = useCompare(repo, valid ? base : "", valid ? head : "")
  const go = (b: string, h: string) =>
    navigate({
      to: "/$",
      params: { _splat: href(repo, "compare", `${b}...${h}`) },
    })

  return (
    <div className="grid grid-cols-1 gap-6">
      <PageTitle>Open a pull request</PageTitle>
      <div className="flex flex-wrap items-center gap-2 text-[13px]">
        <BranchSelect
          label="into"
          value={base}
          refs={refs}
          onChange={(b) => go(b, head)}
        />
        <ArrowLeft className="size-4 text-muted-foreground" />
        <BranchSelect
          label="from"
          value={head}
          refs={refs}
          onChange={(h) => go(base, h)}
        />
      </div>
      {!valid ? (
        <p className="border-t border-hairline py-4 text-[13px] text-muted-foreground">
          Choose two different branches.
        </p>
      ) : q.error ? (
        <QueryError error={q.error} />
      ) : !q.data ? (
        <Skeleton className="h-64" />
      ) : q.data.commits.length === 0 ? (
        <p className="border-t border-hairline py-4 text-[13px] text-muted-foreground">
          <span className="font-mono">{base}</span> already contains every
          commit of <span className="font-mono">{head}</span>.
        </p>
      ) : (
        <>
          <CreateForm
            repo={repo}
            base={base}
            head={head}
            commits={q.data.commits}
          />
          <section>
            <SectionHead title="Commits" count={q.data.commits.length} />
            <CommitRows repo={repo} commits={q.data.commits} />
          </section>
          <SectionHead title="Files changed" className="-mb-4" />
          <ChangedFiles
            repo={repo}
            diff={q.data.diff}
            truncated={q.data.truncated}
            oldRev={q.data.merge_base}
            newRev={q.data.head}
          />
        </>
      )}
    </div>
  )
}

function BranchSelect({
  label,
  value,
  refs,
  onChange,
}: {
  label: string
  value: string
  refs: Refs
  onChange: (branch: string) => void
}) {
  return (
    <label className="flex items-center gap-2">
      <span className="text-muted-foreground">{label}</span>
      <Select value={value} onValueChange={onChange}>
        <SelectTrigger size="sm" className="max-w-56 font-mono text-xs">
          <GitBranch />
          <SelectValue placeholder="branch" />
        </SelectTrigger>
        <SelectContent>
          {refs.branches.map((b) => (
            <SelectItem key={b.name} value={b.name} className="font-mono">
              {b.name}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </label>
  )
}

function CreateForm({
  repo,
  base,
  head,
  commits,
}: {
  repo: string
  base: string
  head: string
  commits: Commit[]
}) {
  const state = useLive()
  const navigate = useNavigate()
  const create = usePullChange(repo)
  // One commit names the pull request; otherwise the branch does.
  const [title, setTitle] = useState(() =>
    commits.length === 1 ? commits[0].subject : head.replace(/[-_/]+/g, " ")
  )
  const [body, setBody] = useState("")
  // Kept for retries of the same pull request.
  const [request] = useState(requestId)

  if (!state?.user) {
    return (
      <p className="rounded-lg border p-4 text-sm">
        <Link
          to="/-/sign-in"
          search={{ next: location.pathname }}
          className="font-medium underline"
        >
          Sign in
        </Link>{" "}
        to open a pull request.
      </p>
    )
  }
  return (
    <form
      className="grid grid-cols-1 gap-3 rounded-[9px] border p-4"
      onSubmit={(e) => {
        e.preventDefault()
        create.mutate(
          {
            op: "pull_create",
            source: head,
            target: base,
            title,
            body,
            request,
          },
          {
            onSuccess: ({ pull }) =>
              navigate({
                to: "/$",
                params: { _splat: href(repo, "pull", String(pull.number)) },
              }),
          }
        )
      }}
    >
      <Input
        aria-label="Title"
        placeholder="Title"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        required
        maxLength={256}
      />
      <MessageEditor
        repo={repo}
        rev={base}
        label="Description"
        placeholder="Describe the changes"
        value={body}
        onChange={setBody}
        className="min-h-28"
      />
      {create.error && (
        <Alert variant="destructive">
          <AlertDescription>{create.error.message}</AlertDescription>
        </Alert>
      )}
      <Button
        type="submit"
        className="justify-self-end"
        disabled={create.isPending || title.trim() === ""}
      >
        {create.isPending ? "Opening…" : "Open pull request"}
      </Button>
    </form>
  )
}

/** Commits oldest first, as they were made. */
export function CommitRows({
  repo,
  commits,
}: {
  repo: string
  commits: Commit[]
}) {
  return (
    <ul className={rowList}>
      {[...commits].reverse().map((c) => (
        <li
          key={c.oid}
          className="flex h-10 items-center gap-3 px-1 text-[13px]"
        >
          <Avatar name={c.author.name} />
          <Link
            to="/$"
            params={{ _splat: href(repo, "commit", c.oid) }}
            className="min-w-0 flex-1 truncate hover:underline"
          >
            {c.subject}
          </Link>
          <span className="hidden shrink-0 text-xs text-muted-foreground sm:inline">
            <AuthorName author={c.author} />, {ago(c.committer.time)}
          </span>
          <Link
            to="/$"
            params={{ _splat: href(repo, "commit", c.oid) }}
            className="shrink-0 font-mono text-xs text-muted-foreground hover:underline"
          >
            {short(c.oid)}
          </Link>
        </li>
      ))}
    </ul>
  )
}
