import { Link } from "@tanstack/react-router"
import {
  AlertTriangle,
  CheckCircle2,
  ChevronDown,
  ExternalLink,
  FileDiff,
  GitCommitHorizontal,
  GitMerge,
  GitPullRequestClosed,
  MessageSquare,
  RotateCcw,
  Upload,
} from "lucide-react"
import { useState, type ReactNode } from "react"

import {
  requestId,
  useCompare,
  useLive,
  usePull,
  usePullChange,
  type Pull,
  type PullDetail,
  type PullEvent,
} from "@/lib/api"
import { ago, fullDate, short } from "@/lib/format"
import { href } from "@/lib/paths"
import { cn } from "@/lib/utils"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { Skeleton } from "@/components/ui/skeleton"
import { QueryError } from "@/components/query-error"
import { Avatar } from "./avatar"
import { ChangedFiles } from "./changes"
import { CommitRows } from "./compare"
import { MessageView } from "./markdown"
import { MessageEditor } from "./message-editor"
import { BranchName, PullStateBadge } from "./pulls"

type Tab = "conversation" | "commits" | "files"

export function PullView({ repo, rest }: { repo: string; rest: string[] }) {
  const n = Number(rest[0])
  // 0 is no pull request; the server answers that it is not found.
  const number = Number.isSafeInteger(n) && n > 0 ? n : 0
  const tab: Tab =
    rest[1] === "commits" || rest[1] === "files" ? rest[1] : "conversation"
  const q = usePull(repo, number)
  const d = q.data
  // The changes come from the repository, not the database.
  const cmp = useCompare(repo, d?.base ?? "", d?.head ?? "")

  if (q.error) return <QueryError error={q.error} />
  if (!d) return <Skeleton className="h-96" />
  const { pull } = d
  const commits = cmp.data?.commits
  const github = !!pull.github_url
  // Who merged a pull request from GitHub is known only for merges seen
  // after the first import.
  const subject =
    pull.state === "merged"
      ? (pull.merged_by ?? (github ? null : "A push"))
      : pull.author

  return (
    <div className="grid grid-cols-1 gap-6">
      <div className="grid grid-cols-1 gap-3">
        <div className="flex flex-wrap items-start gap-3">
          <h2 className="min-w-0 flex-1 text-2xl break-words">
            {pull.title}{" "}
            <span className="font-light text-muted-foreground">
              #{pull.number}
            </span>
          </h2>
          {pull.github_url && (
            <Button asChild variant="outline" size="sm">
              <a href={pull.github_url} target="_blank" rel="noreferrer">
                <ExternalLink />
                View on GitHub
              </a>
            </Button>
          )}
        </div>
        <div className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
          <PullStateBadge state={pull.state} />
          <span>
            {subject === null ? (
              "Merged"
            ) : (
              <>
                <Who name={subject} pull={pull} server={subject === "A push"} />{" "}
                {pull.state === "merged" ? "merged" : "wants to merge"}
              </>
            )}{" "}
            {commits && (
              <>
                {commits.length}
                {cmp.data?.more && "+"}{" "}
                {commits.length === 1 ? "commit" : "commits"}{" "}
              </>
            )}
            into <BranchName name={pull.target} /> from{" "}
            <BranchName name={pull.source} />
          </span>
        </div>
      </div>
      <PullTabs
        repo={repo}
        number={pull.number}
        tab={tab}
        commits={commits?.length}
        files={cmp.data && countFiles(cmp.data.diff)}
      />
      {tab === "conversation" ? (
        <Conversation repo={repo} d={d} />
      ) : cmp.error ? (
        <QueryError error={cmp.error} />
      ) : !cmp.data ? (
        <Skeleton className="h-64" />
      ) : tab === "commits" ? (
        <CommitRows repo={repo} commits={cmp.data.commits} />
      ) : (
        <ChangedFiles
          repo={repo}
          diff={cmp.data.diff}
          truncated={cmp.data.truncated}
          oldRev={cmp.data.merge_base}
          newRev={cmp.data.head}
          tree="lager-pull-files"
        />
      )}
    </div>
  )
}

function countFiles(diff: string) {
  return diff.match(/^diff --git /gm)?.length ?? 0
}

function PullTabs({
  repo,
  number,
  tab,
  commits,
  files,
}: {
  repo: string
  number: number
  tab: Tab
  commits?: number
  files?: number
}) {
  const tabs: {
    tab: Tab
    label: string
    icon: typeof GitMerge
    count?: number
  }[] = [
    { tab: "conversation", label: "Conversation", icon: MessageSquare },
    {
      tab: "commits",
      label: "Commits",
      icon: GitCommitHorizontal,
      count: commits,
    },
    { tab: "files", label: "Files changed", icon: FileDiff, count: files },
  ]
  return (
    <nav className="-mt-2 flex [scrollbar-width:none] gap-1 overflow-x-auto border-b">
      {tabs.map((t) => (
        <Link
          key={t.tab}
          to="/$"
          params={{
            _splat: href(
              repo,
              "pull",
              String(number),
              t.tab === "conversation" ? undefined : t.tab
            ),
          }}
          className={cn(
            "-mb-px flex shrink-0 items-center gap-2 border-b-2 px-2.5 py-2 text-sm whitespace-nowrap sm:px-3",
            tab === t.tab
              ? "border-foreground font-medium"
              : "border-transparent text-muted-foreground hover:text-foreground"
          )}
        >
          <t.icon className="hidden size-4 sm:block" />
          {t.label}
          {t.count !== undefined && (
            <span className="rounded-full bg-muted px-1.5 text-xs">
              {t.count}
            </span>
          )}
        </Link>
      ))}
    </nav>
  )
}

/** The GitHub repository of a pull request's page on GitHub. */
function githubRepo(url: string | null) {
  const m = url?.match(/^https:\/\/github\.com\/([^/]+\/[^/]+)\/pull\//)
  return m ? m[1] : null
}

/**
 * A user, or for a pull request from GitHub a GitHub account. `server`:
 * not a name ("A push").
 */
function Who({
  name,
  pull,
  server,
  className,
}: {
  name: string
  pull: Pull
  server?: boolean
  className?: string
}) {
  const cls = cn("font-medium text-foreground", className)
  if (pull.github_url && !server)
    return (
      <a
        href={`https://github.com/${encodeURIComponent(name)}`}
        target="_blank"
        rel="noreferrer"
        className={cn(cls, "hover:underline")}
      >
        {name}
      </a>
    )
  return <span className={cls}>{name}</span>
}

function Conversation({ repo, d }: { repo: string; d: PullDetail }) {
  const { pull } = d
  return (
    <div className="grid grid-cols-1 gap-4">
      <CommentCard
        repo={repo}
        pull={pull}
        author={pull.author}
        time={pull.created_ms}
        body={pull.body}
        empty="No description."
      />
      {d.events.map((e) => (
        <TimelineEvent key={e.id} repo={repo} pull={pull} e={e} />
      ))}
      {pull.state === "open" && !pull.github_url && (
        <MergeBox repo={repo} d={d} />
      )}
      {pull.github_url ? (
        <p className="rounded-lg border p-4 text-sm">
          This pull request is on GitHub, and this copy follows it.{" "}
          <a
            href={pull.github_url}
            target="_blank"
            rel="noreferrer"
            className="font-medium underline"
          >
            Comment on GitHub
          </a>
          .
        </p>
      ) : (
        <CommentForm repo={repo} d={d} />
      )}
    </div>
  )
}

function CommentCard({
  repo,
  pull,
  author,
  time,
  edited,
  body,
  empty,
}: {
  repo: string
  pull: Pull
  author: string
  time: number
  edited?: number | null
  body: string
  empty?: string
}) {
  return (
    <div className="flex gap-3">
      <Avatar name={author} className="mt-2 size-8 text-xs" />
      <div className="min-w-0 flex-1 rounded-lg border">
        <div className="border-b bg-muted/40 px-4 py-2 text-sm">
          <Who name={author} pull={pull} />{" "}
          <span className="text-muted-foreground" title={fullDate(time / 1000)}>
            commented {ago(time / 1000)}
          </span>
          {edited && (
            <span
              className="text-muted-foreground"
              title={`Edited ${fullDate(edited / 1000)}`}
            >
              {" "}
              · edited
            </span>
          )}
        </div>
        <div className="p-4 text-sm">
          {body.trim() ? (
            <MessageView
              repo={repo}
              rev={pull.target}
              github={githubRepo(pull.github_url)}
              text={body}
            />
          ) : (
            <span className="text-muted-foreground italic">{empty}</span>
          )}
        </div>
      </div>
    </div>
  )
}

function TimelineEvent({
  repo,
  pull,
  e,
}: {
  repo: string
  pull: Pull
  e: PullEvent
}) {
  if (e.kind === "comment") {
    return (
      <CommentCard
        repo={repo}
        pull={pull}
        author={e.author ?? "?"}
        time={e.created_ms}
        edited={e.edited_ms}
        body={e.body ?? ""}
      />
    )
  }
  // Events copied from GitHub do not say who closed or reopened.
  const who = (verb: string) =>
    e.author ? (
      <>
        <Who name={e.author} pull={pull} /> {verb} this
      </>
    ) : (
      <>{verb[0].toUpperCase() + verb.slice(1)} on GitHub</>
    )
  const commit = (oid: string | null) =>
    oid && (
      <Link
        to="/$"
        params={{ _splat: href(repo, "commit", oid) }}
        className="font-mono text-xs hover:underline"
      >
        {short(oid)}
      </Link>
    )
  const line = (icon: ReactNode, text: ReactNode) => (
    <div className="flex items-center gap-3 pl-1.5 text-sm text-muted-foreground">
      <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-muted">
        {icon}
      </span>
      <span className="min-w-0">
        {text}{" "}
        <span title={fullDate(e.created_ms / 1000)}>
          {ago(e.created_ms / 1000)}
        </span>
      </span>
    </div>
  )
  switch (e.kind) {
    case "push":
      return line(
        <Upload className="size-3.5" />,
        <>
          The source branch moved from {commit(e.old_oid)} to {commit(e.oid)}
        </>
      )
    case "closed":
      return line(
        <GitPullRequestClosed className="size-3.5 text-red-600" />,
        who("closed")
      )
    case "reopened":
      return line(<RotateCcw className="size-3.5" />, who("reopened"))
    case "merged":
      return line(
        <GitMerge className="size-3.5 text-violet-600" />,
        <>
          {e.author ? (
            <>
              <Who name={e.author} pull={pull} /> merged this
            </>
          ) : pull.github_url ? (
            "Merged on GitHub"
          ) : (
            "A push merged this"
          )}{" "}
          as {commit(e.oid)}
        </>
      )
  }
}

const mergeStatus: Record<
  NonNullable<PullDetail["mergeable"]>,
  { ok: boolean; title: string; text: string }
> = {
  clean: {
    ok: true,
    title: "No conflicts with the target branch",
    text: "The branches merge without conflicts.",
  },
  conflict: {
    ok: false,
    title: "The branches conflict",
    text: "Merge the target branch into the source branch, resolve the conflicts, and push.",
  },
  up_to_date: {
    ok: true,
    title: "Already merged",
    text: "The target branch contains every commit; the pull request is marked merged shortly.",
  },
  source_missing: {
    ok: false,
    title: "The source branch was deleted",
    text: "Push the branch again to merge it.",
  },
  target_missing: {
    ok: false,
    title: "The target branch was deleted",
    text: "Push the branch again to merge into it.",
  },
}

function MergeBox({ repo, d }: { repo: string; d: PullDetail }) {
  const status = mergeStatus[d.mergeable ?? "clean"]
  const change = usePullChange(repo)
  const [squash, setSquash] = useState(false)
  // One merge per page state: a retry repeats the same request.
  const [request, setRequest] = useState(requestId)
  const canMerge = d.can_merge && d.mergeable === "clean"
  return (
    <div className="flex gap-3">
      <span
        className={cn(
          "mt-2 flex size-8 shrink-0 items-center justify-center rounded-full text-white",
          status.ok ? "bg-emerald-600" : "bg-amber-600"
        )}
      >
        {status.ok ? (
          <CheckCircle2 className="size-4" />
        ) : (
          <AlertTriangle className="size-4" />
        )}
      </span>
      <div className="min-w-0 flex-1 rounded-lg border">
        <div className="p-4">
          <p className="font-medium">{status.title}</p>
          <p className="text-sm text-muted-foreground">{status.text}</p>
          {d.conflicts.length > 0 && (
            <ul className="mt-2 font-mono text-xs text-muted-foreground">
              {d.conflicts.map((f) => (
                <li key={f}>{f}</li>
              ))}
            </ul>
          )}
        </div>
        {d.can_merge && (
          <div className="flex flex-wrap items-center gap-2 border-t bg-muted/40 px-4 py-3">
            <div
              role="group"
              aria-label="Merge pull request"
              className="inline-flex"
            >
              <Button
                size="sm"
                className="rounded-r-none focus-visible:z-10"
                disabled={!canMerge || change.isPending}
                onClick={() =>
                  change.mutate(
                    {
                      op: "pull_merge",
                      number: d.pull.number,
                      head: d.head,
                      squash,
                      request,
                    },
                    // A refused merge leaves nothing to repeat.
                    { onError: () => setRequest(requestId()) }
                  )
                }
              >
                {squash ? <GitCommitHorizontal /> : <GitMerge />}
                {change.isPending
                  ? "Merging…"
                  : squash
                    ? "Squash and merge"
                    : "Merge pull request"}
              </Button>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button
                    size="icon-sm"
                    className="rounded-l-none border-l-primary-foreground/25 focus-visible:z-10"
                    aria-label="Choose merge method"
                    disabled={!canMerge || change.isPending}
                  >
                    <ChevronDown />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="w-56">
                  <DropdownMenuRadioGroup
                    value={squash ? "squash" : "merge"}
                    onValueChange={(v) => setSquash(v === "squash")}
                  >
                    <DropdownMenuRadioItem value="merge">
                      Create a merge commit
                    </DropdownMenuRadioItem>
                    <DropdownMenuRadioItem value="squash">
                      Squash into one commit
                    </DropdownMenuRadioItem>
                  </DropdownMenuRadioGroup>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
            {change.error && (
              <p className="basis-full text-sm text-destructive">
                {change.error.message}
              </p>
            )}
          </div>
        )}
      </div>
    </div>
  )
}

function CommentForm({ repo, d }: { repo: string; d: PullDetail }) {
  const state = useLive()
  const change = usePullChange(repo)
  const [body, setBody] = useState("")
  const [request, setRequest] = useState(requestId)
  const { pull } = d
  if (!state?.user || !d.can_comment) {
    return (
      <p className="rounded-lg border p-4 text-sm">
        <Link
          to="/-/sign-in"
          search={{ next: location.pathname }}
          className="font-medium underline"
        >
          Sign in
        </Link>{" "}
        to comment.
      </p>
    )
  }
  const toggle = pull.state === "open" ? "pull_close" : "pull_reopen"
  return (
    <form
      className="flex gap-3"
      onSubmit={(e) => {
        e.preventDefault()
        change.mutate(
          { op: "pull_comment", number: pull.number, body, request },
          {
            onSuccess: () => {
              setBody("")
              setRequest(requestId())
            },
          }
        )
      }}
    >
      <Avatar name={state.user} className="mt-2 size-8 text-xs" />
      <div className="grid min-w-0 flex-1 grid-cols-1 gap-2">
        <MessageEditor
          repo={repo}
          rev={pull.target}
          label="Comment"
          placeholder="Leave a comment"
          value={body}
          onChange={setBody}
          className="min-h-24"
        />
        {change.error && (
          <Alert variant="destructive">
            <AlertDescription>{change.error.message}</AlertDescription>
          </Alert>
        )}
        <div className="flex justify-end gap-2">
          {d.can_close && pull.state !== "merged" && (
            <Button
              type="button"
              variant="outline"
              disabled={change.isPending}
              onClick={() => change.mutate({ op: toggle, number: pull.number })}
            >
              {pull.state === "open" ? (
                <>
                  <GitPullRequestClosed className="text-red-600" />
                  Close pull request
                </>
              ) : (
                <>
                  <RotateCcw />
                  Reopen pull request
                </>
              )}
            </Button>
          )}
          <Button
            type="submit"
            disabled={change.isPending || body.trim() === ""}
          >
            Comment
          </Button>
        </div>
      </div>
    </form>
  )
}
