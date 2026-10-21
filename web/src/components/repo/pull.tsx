import { Link } from "@tanstack/react-router"
import {
  AlertTriangle,
  CheckCircle2,
  ChevronDown,
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
import { Textarea } from "@/components/ui/textarea"
import { QueryError } from "@/components/query-error"
import { Avatar } from "./avatar"
import { ChangedFiles } from "./changes"
import { CommitRows } from "./compare"
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

  return (
    <div className="grid grid-cols-1 gap-6">
      <div className="grid grid-cols-1 gap-3">
        <h2 className="text-2xl break-words">
          {pull.title}{" "}
          <span className="font-light text-muted-foreground">
            #{pull.number}
          </span>
        </h2>
        <div className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
          <PullStateBadge state={pull.state} />
          <span>
            <span className="font-medium text-foreground">
              {pull.state === "merged"
                ? (pull.merged_by ?? "A push")
                : pull.author}
            </span>{" "}
            {pull.state === "merged" ? "merged" : "wants to merge"}{" "}
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

function Conversation({ repo, d }: { repo: string; d: PullDetail }) {
  const { pull } = d
  return (
    <div className="grid grid-cols-1 gap-4">
      <CommentCard
        author={pull.author}
        time={pull.created_ms}
        body={pull.body}
        empty="No description."
      />
      {d.events.map((e) => (
        <TimelineEvent key={e.id} repo={repo} e={e} />
      ))}
      {pull.state === "open" && <MergeBox repo={repo} d={d} />}
      <CommentForm repo={repo} d={d} />
    </div>
  )
}

function CommentCard({
  author,
  time,
  body,
  empty,
}: {
  author: string
  time: number
  body: string
  empty?: string
}) {
  return (
    <div className="flex gap-3">
      <Avatar name={author} className="mt-2 size-8 text-xs" />
      <div className="min-w-0 flex-1 rounded-lg border">
        <div className="border-b bg-muted/40 px-4 py-2 text-sm">
          <span className="font-medium">{author}</span>{" "}
          <span className="text-muted-foreground" title={fullDate(time / 1000)}>
            commented {ago(time / 1000)}
          </span>
        </div>
        <div className="p-4 text-sm break-words whitespace-pre-wrap">
          {body.trim() || (
            <span className="text-muted-foreground italic">{empty}</span>
          )}
        </div>
      </div>
    </div>
  )
}

function TimelineEvent({ repo, e }: { repo: string; e: PullEvent }) {
  if (e.kind === "comment") {
    return (
      <CommentCard
        author={e.author ?? "?"}
        time={e.created_ms}
        body={e.body ?? ""}
      />
    )
  }
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
        <>
          <span className="font-medium text-foreground">{e.author}</span> closed
          this
        </>
      )
    case "reopened":
      return line(
        <RotateCcw className="size-3.5" />,
        <>
          <span className="font-medium text-foreground">{e.author}</span>{" "}
          reopened this
        </>
      )
    case "merged":
      return line(
        <GitMerge className="size-3.5 text-violet-600" />,
        <>
          {e.author ? (
            <span className="font-medium text-foreground">{e.author}</span>
          ) : (
            "A push"
          )}{" "}
          merged this as {commit(e.oid)}
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
        <Textarea
          aria-label="Comment"
          placeholder="Leave a comment"
          value={body}
          onChange={(e) => setBody(e.target.value)}
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
