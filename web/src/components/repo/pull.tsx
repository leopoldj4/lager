import { Link } from "@tanstack/react-router"
import {
  AlertTriangle,
  ArrowRightLeft,
  Check,
  CheckCircle2,
  ChevronDown,
  ExternalLink,
  GitCommitHorizontal,
  GitMerge,
  GitPullRequestClosed,
  Plus,
  RotateCcw,
  Upload,
} from "lucide-react"
import { useEffect, useMemo, useState, type ReactNode } from "react"

import {
  requestId,
  useCompare,
  useLive,
  usePull,
  useRefs,
  usePullChange,
  type Commit,
  type Pull,
  type PullDetail,
  type PullEvent,
  type PullLayer,
} from "@/lib/api"
import { ago, fullDate, short, since } from "@/lib/format"
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
import { ChangedFiles } from "./changes"
import { parseDiff } from "./diff"
import { MessageView } from "./markdown"
import { MessageEditor } from "./message-editor"
import { PullStateBadge } from "./pulls"
import { DiffStat, PageBody, SectionHead } from "@/components/frame"

/**
 * A pull request: its commits as a stack from the head down to the target
 * branch, or inside its layer of a stack of pull requests, the conversation and the changed files one below the other, and
 * the state and properties beside them. `/commits` and `/files` scroll to
 * their section.
 */
export function PullView({ repo, rest }: { repo: string; rest: string[] }) {
  const n = Number(rest[0])
  // 0 is no pull request; the server answers that it is not found.
  const number = Number.isSafeInteger(n) && n > 0 ? n : 0
  const section = rest[1] === "commits" || rest[1] === "files" ? rest[1] : null
  const q = usePull(repo, number)
  const d = q.data
  // The changes come from the repository, not the database.
  const cmp = useCompare(repo, d?.base ?? "", d?.head ?? "")
  const files = useMemo(() => cmp.data && parseDiff(cmp.data.diff), [cmp.data])
  const [tree, setTree] = useState<HTMLElement | null>(null)
  const loaded = !!cmp.data
  useEffect(() => {
    if (section && loaded)
      document.getElementById(section)?.scrollIntoView({ block: "start" })
  }, [section, loaded])

  if (q.error)
    return (
      <PageBody>
        <QueryError error={q.error} />
      </PageBody>
    )
  if (!d)
    return (
      <PageBody>
        <Skeleton className="h-96" />
      </PageBody>
    )
  const { pull } = d
  const commits = cmp.data?.commits
  const adds = files?.reduce((n, f) => n + f.adds, 0) ?? 0
  const dels = files?.reduce((n, f) => n + f.dels, 0) ?? 0
  const stacked = d.stack.length > 1
  // A new pull request on top of this one, or of its stack.
  const add = pull.state === "open" && d.can_comment

  return (
    <div className="flex min-h-[calc(var(--frame-height)-3rem)] shrink-0 flex-col lg:flex-row">
      {/* On phones a stack comes first, the commits of one alone last. */}
      <aside
        className={cn(
          "shrink-0 border-hairline lg:w-75 lg:border-r",
          stacked
            ? "border-b lg:border-b-0"
            : "order-last border-t lg:order-none lg:border-t-0"
        )}
      >
        <div className="grid grid-cols-1 gap-8 px-4 py-6 lg:sticky lg:top-12 lg:max-h-[calc(var(--frame-height)-3rem)] lg:overflow-y-auto">
          <section id="commits" className="scroll-mt-14">
            <div className="flex items-center gap-2 px-1 pb-3">
              <h2 className="label-caps">{stacked ? "Stack" : "Commits"}</h2>
              {stacked ? (
                <span className="font-mono text-[11px] font-medium text-label">
                  {d.stack.length}
                </span>
              ) : (
                commits && (
                  <span className="font-mono text-[11px] font-medium text-label">
                    {commits.length}
                    {cmp.data?.more && "+"}
                  </span>
                )
              )}
              {files && (
                <span className="ml-auto">
                  <DiffStat adds={adds} dels={dels} />
                </span>
              )}
            </div>
            {stacked ? (
              <LayerStack
                repo={repo}
                layers={d.stack}
                current={pull.number}
                add={add}
              >
                {cmp.error ? (
                  <QueryError error={cmp.error} />
                ) : !commits ? (
                  <Skeleton className="h-16" />
                ) : (
                  <LayerCommits
                    repo={repo}
                    commits={commits}
                    more={!!cmp.data?.more}
                  />
                )}
              </LayerStack>
            ) : cmp.error ? (
              <QueryError error={cmp.error} />
            ) : !commits ? (
              <Skeleton className="h-32" />
            ) : (
              <CommitStack
                repo={repo}
                commits={commits}
                more={!!cmp.data?.more}
                target={pull.target}
                base={cmp.data!.merge_base}
                add={add ? pull.source : undefined}
              />
            )}
          </section>
          {files && files.length > 0 && (
            <section className="max-lg:hidden">
              <div className="flex items-center gap-2 px-1 pb-2">
                <h2 className="label-caps">Files</h2>
                <span className="font-mono text-[11px] font-medium text-label">
                  {files.length}
                </span>
              </div>
              <div ref={setTree} />
            </section>
          )}
        </div>
      </aside>
      <div className="grid min-w-0 flex-1 grid-cols-1 content-start gap-8 px-4 py-8 md:px-8">
        <div className="grid gap-3">
          <h1 className="font-display text-[30px] leading-9 break-words text-ink">
            {pull.title}{" "}
            <span className="font-sans text-2xl font-light tracking-normal text-label">
              #{pull.number}
            </span>
          </h1>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-2 text-[13px] text-muted-foreground">
            <PullStateBadge state={pull.state} />
            <span>
              <Who
                name={pull.author}
                pull={pull}
                className="font-normal text-muted-foreground"
              />
              {commits && (
                <>
                  {" · "}
                  {commits.length}
                  {cmp.data?.more && "+"}{" "}
                  {commits.length === 1 ? "commit" : "commits"}
                </>
              )}
              {" · "}
              <span title={fullDate(pull.created_ms / 1000)}>
                {since(pull.created_ms / 1000)}
              </span>
            </span>
            {/* Beside the page from xl on. */}
            <span className="flex min-w-0 items-center gap-1.5 font-mono text-[11px] text-body xl:hidden">
              <span className="truncate">{pull.source}</span>
              <span className="text-label">→</span>
              <span className="truncate">{pull.target}</span>
            </span>
          </div>
        </div>
        <Conversation repo={repo} d={d} />
        <section id="files" className="scroll-mt-14">
          <SectionHead title="Files changed" count={files?.length} />
          {cmp.error ? (
            <QueryError error={cmp.error} />
          ) : !cmp.data ? (
            <Skeleton className="h-64" />
          ) : (
            <ChangedFiles
              repo={repo}
              diff={cmp.data.diff}
              truncated={cmp.data.truncated}
              oldRev={cmp.data.merge_base}
              newRev={cmp.data.head}
              tree={tree}
            />
          )}
        </section>
      </div>
      <aside className="order-last shrink-0 border-t border-hairline px-4 py-6 lg:order-none max-xl:lg:hidden xl:w-66 xl:border-t-0 xl:border-l">
        <div className="grid gap-6 xl:sticky xl:top-18">
          <Stepper pull={pull} />
          <PullProperties repo={repo} d={d} files={files?.length} />
        </div>
      </aside>
    </div>
  )
}

const rail = <span className="ml-[22px] block h-2.5 w-px bg-stage-off" />
const card =
  "rounded-[7px] bg-card px-3 py-2.5 shadow-[0_0_0_1px_var(--border),0_1px_2px_#0000000a] hover:shadow-[0_0_0_1px_var(--stage-off),0_1px_3px_#00000014]"
const selected = "shadow-[0_0_0_1.5px_var(--primary)]!"

/** The number of a card on the rail, counted from the bottom. */
function Position({ n, on }: { n: ReactNode; on: boolean }) {
  return (
    <span
      className={cn(
        "flex size-5 items-center justify-center rounded-[4px] font-mono text-[10px] font-semibold",
        on
          ? "bg-primary text-primary-foreground"
          : "border text-muted-foreground"
      )}
    >
      {n}
    </span>
  )
}

/** Where the rail ends: the branch below the lowest card. */
function RailEnd({ branch, base }: { branch: string; base?: string }) {
  return (
    <div className="flex items-center gap-2.5 py-1 pl-4">
      <span className="size-2.5 rounded-[2px] border-[1.5px] border-label" />
      <span className="truncate font-mono text-xs text-body">{branch}</span>
      {base && (
        <span className="font-mono text-[11px] text-label">
          {short(base).slice(0, 7)}
        </span>
      )}
    </div>
  )
}

/** The slot above the top card: opens a pull request on top of `branch`. */
function AddLayer({ repo, branch }: { repo: string; branch: string }) {
  return (
    <li>
      <Link
        to="/$"
        params={{ _splat: href(repo, "compare", `${branch}...`) }}
        className="flex h-9 items-center gap-2.5 rounded-[7px] border border-dashed border-stage-off px-3 text-[13px] text-muted-foreground hover:border-label hover:text-ink"
      >
        <Plus className="size-3.5" />
        Add layer
      </Link>
      {rail}
    </li>
  )
}

/**
 * The pull requests of a stack from the top down, as cards on a rail that
 * ends at the base branch; numbered from the bottom. The card of this pull
 * request has the ring and holds `children`, its commits.
 */
function LayerStack({
  repo,
  layers,
  current,
  add,
  children,
}: {
  repo: string
  layers: PullLayer[]
  current: number
  add: boolean
  children: ReactNode
}) {
  return (
    <ol>
      {add && <AddLayer repo={repo} branch={layers[0].source} />}
      {layers.map((l, i) => {
        const on = l.number === current
        const head = (
          <span className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-2.5 gap-y-1.5">
            <Position n={layers.length - i} on={on} />
            <span className="truncate text-[13px] font-medium text-ink">
              {l.title}
            </span>
            <span />
            <span className="flex items-center gap-2 font-mono text-[11px] text-label">
              #{l.number}
              <span className="truncate" title={l.source}>
                {l.source}
              </span>
            </span>
          </span>
        )
        return (
          <li key={l.number}>
            {i > 0 && rail}
            {on ? (
              <div className={cn(card, selected)} aria-current="true">
                {head}
                {children}
              </div>
            ) : (
              <Link
                to="/$"
                params={{ _splat: href(repo, "pull", String(l.number)) }}
                className={cn(card, "block")}
              >
                {head}
              </Link>
            )}
          </li>
        )
      })}
      <li>
        {rail}
        <RailEnd branch={layers[layers.length - 1].target} />
      </li>
    </ol>
  )
}

/** The commits of a layer from the head down, as rows inside its card. */
function LayerCommits({
  repo,
  commits,
  more,
}: {
  repo: string
  commits: Commit[]
  more: boolean
}) {
  if (commits.length === 0) return null
  return (
    <ol className="mt-2.5 border-t border-hairline pt-1.5">
      {commits.map((c, i) => (
        <li key={c.oid}>
          <Link
            to="/$"
            params={{ _splat: href(repo, "commit", c.oid) }}
            className="-mx-1.5 flex h-7 items-center gap-2.5 rounded-[5px] px-1.5 hover:bg-muted/50"
            title={c.subject}
          >
            <span className="w-5 shrink-0 text-center font-mono text-[10px] font-semibold text-label">
              {more ? "" : commits.length - i}
            </span>
            <span className="min-w-0 truncate text-[13px] text-body">
              {c.subject}
            </span>
            <span className="ml-auto shrink-0 font-mono text-[11px] text-label">
              {short(c.oid).slice(0, 7)}
            </span>
          </Link>
        </li>
      ))}
    </ol>
  )
}

/**
 * The commits from the head down, as cards on a rail that ends at the
 * target branch; numbered from the bottom, the head in blue.
 */
function CommitStack({
  repo,
  commits,
  more,
  target,
  base,
  add,
}: {
  repo: string
  commits: Commit[]
  more: boolean
  target: string
  base: string
  /** The source branch, to offer a pull request on top of it. */
  add?: string
}) {
  return (
    <ol>
      {add && <AddLayer repo={repo} branch={add} />}
      {commits.map((c, i) => (
        <li key={c.oid}>
          {i > 0 && rail}
          <Link
            to="/$"
            params={{ _splat: href(repo, "commit", c.oid) }}
            className={cn(
              card,
              "grid grid-cols-[auto_1fr] gap-x-2.5 gap-y-1.5",
              i === 0 && selected
            )}
          >
            <Position n={more ? "" : commits.length - i} on={i === 0} />
            <span className="truncate text-[13px] font-medium text-ink">
              {c.subject}
            </span>
            <span />
            <span className="flex items-center justify-between font-mono text-[11px] text-label">
              {short(c.oid).slice(0, 7)}
              <span
                className="font-sans text-xs"
                title={fullDate(c.committer.time)}
              >
                {since(c.committer.time)}
              </span>
            </span>
          </Link>
        </li>
      ))}
      <li>
        {commits.length > 0 && rail}
        <RailEnd branch={target} base={base} />
      </li>
    </ol>
  )
}

/** Opened, then merged or closed: done steps in ink, the current one tinted. */
function Stepper({ pull }: { pull: Pull }) {
  const end = pull.closed_ms ?? pull.updated_ms
  const steps: {
    label: string
    time?: number
    state: "done" | "now" | "next"
  }[] =
    pull.state === "open"
      ? [
          { label: "Open", time: pull.created_ms, state: "now" },
          { label: "Merged", state: "next" },
        ]
      : [
          { label: "Open", time: pull.created_ms, state: "done" },
          {
            label: pull.state === "merged" ? "Merged" : "Closed",
            time: end,
            state: "done",
          },
        ]
  return (
    <ol className="grid">
      {steps.map((s, i) => (
        <li key={s.label}>
          {i > 0 && (
            <span
              className={cn(
                "ml-[17px] block h-3 w-0.5",
                s.state === "next" ? "bg-stage-off" : "bg-ink"
              )}
            />
          )}
          <div
            className={cn(
              "flex h-10 items-center gap-3 rounded-md px-2.5 text-[13px]",
              s.state === "now" && "bg-tint font-medium text-ink"
            )}
          >
            <span
              className={cn(
                "flex size-4 shrink-0 items-center justify-center rounded-[4px]",
                s.state === "done" && "bg-ink text-background",
                s.state === "now" && "bg-primary",
                s.state === "next" && "border border-dashed border-label"
              )}
            >
              {s.state === "done" && (
                <Check className="size-3" strokeWidth={3} />
              )}
            </span>
            <span
              className={cn(
                s.state === "next"
                  ? "text-label"
                  : s.state === "done" && "text-muted-foreground"
              )}
            >
              {s.label}
            </span>
            {s.time && (
              <span
                className={cn(
                  "ml-auto text-xs",
                  s.state === "now" ? "text-tint-foreground" : "text-label"
                )}
                title={fullDate(s.time / 1000)}
              >
                {new Date(s.time).toLocaleDateString(undefined, {
                  month: "short",
                  day: "numeric",
                })}
              </span>
            )}
          </div>
        </li>
      ))}
    </ol>
  )
}

/** The target branch of an open pull request, as a picker that moves it. */
function TargetPicker({ repo, pull }: { repo: string; pull: Pull }) {
  const refs = useRefs(repo)
  const change = usePullChange(repo)
  const branches = (refs.data?.branches ?? [])
    .map((b) => b.name)
    .filter((b) => b !== pull.source)
  return (
    <>
      <Select
        value={pull.target}
        disabled={change.isPending}
        onValueChange={(target) =>
          change.mutate({ op: "pull_retarget", number: pull.number, target })
        }
      >
        <SelectTrigger
          size="sm"
          aria-label="Target branch"
          className="h-5.5! max-w-full min-w-0 gap-1.25 rounded-[5px] px-2 font-mono text-[11px] text-ink shadow-none [&_svg]:size-3! [&_svg]:text-muted-foreground"
        >
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {!branches.includes(pull.target) && (
            <SelectItem value={pull.target} className="font-mono">
              {pull.target}
            </SelectItem>
          )}
          {branches.map((b) => (
            <SelectItem key={b} value={b} className="font-mono">
              {b}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      {change.error && (
        <p className="mt-1.5 text-xs text-destructive">
          {change.error.message}
        </p>
      )}
    </>
  )
}

function PullProperties({
  repo,
  d,
  files,
}: {
  repo: string
  d: PullDetail
  files?: number
}) {
  const { pull } = d
  // Who merged a pull request from GitHub is known only for merges seen
  // after the first import.
  const merger =
    pull.state === "merged"
      ? (pull.merged_by ?? (pull.github_url ? null : "A push"))
      : null
  return (
    <dl className="grid grid-cols-[5rem_minmax(0,1fr)] items-center gap-y-3.5 border-t border-hairline pt-6 text-xs">
      <dt className="text-label">Author</dt>
      <dd className="flex min-w-0 items-center gap-2 text-[13px]">
        <Avatar name={pull.author} className="size-5 text-[9px]" />
        <Who name={pull.author} pull={pull} className="truncate" />
      </dd>
      <dt className="text-label">Branch</dt>
      <dd className="truncate font-mono text-xs text-ink" title={pull.source}>
        {pull.source}
      </dd>
      <dt className="text-label">Into</dt>
      {pull.state === "open" && d.can_close ? (
        <dd className="min-w-0">
          <TargetPicker repo={repo} pull={pull} />
        </dd>
      ) : (
        <dd className="truncate font-mono text-xs text-ink" title={pull.target}>
          {pull.target}
        </dd>
      )}
      {files !== undefined && (
        <>
          <dt className="text-label">Files</dt>
          <dd className="text-[13px] font-medium text-ink">{files}</dd>
        </>
      )}
      {merger && (
        <>
          <dt className="text-label">Merged by</dt>
          <dd className="truncate text-[13px]">
            <Who name={merger} pull={pull} server={merger === "A push"} />
          </dd>
        </>
      )}
      {pull.github_url && (
        <>
          <dt className="text-label">GitHub</dt>
          <dd>
            <a
              href={pull.github_url}
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center gap-1 text-[13px] font-medium text-ink hover:underline"
            >
              View
              <ExternalLink className="size-3 text-label" />
            </a>
          </dd>
        </>
      )}
    </dl>
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
  if (server || name === "?") return <span className={cls}>{name}</span>
  return (
    <Link
      to="/$"
      params={{ _splat: name }}
      className={cn(cls, "hover:underline")}
    >
      {name}
    </Link>
  )
}

function Conversation({ repo, d }: { repo: string; d: PullDetail }) {
  const state = useLive()
  const { pull } = d
  // A copy whose repository no longer follows GitHub changes only there.
  const frozen = !!pull.github_url && !!state?.user && !d.can_comment
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
      {pull.state === "open" && (!pull.github_url || d.mergeable) && (
        <MergeBox repo={repo} d={d} />
      )}
      {frozen ? (
        <p className="rounded-lg border p-4 text-sm">
          This pull request is on GitHub.{" "}
          <a
            href={pull.github_url!}
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
      <Avatar name={author} className="mt-2 size-7 text-[11px]" />
      <div className="min-w-0 flex-1 rounded-[9px] border">
        <div className="border-b border-hairline px-4 py-2 text-[13px]">
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
    <div className="flex items-center gap-3 pl-0.5 text-[13px] text-muted-foreground">
      <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-muted text-muted-foreground">
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
        <GitPullRequestClosed className="size-3.5 text-removed" />,
        who("closed")
      )
    case "reopened":
      return line(<RotateCcw className="size-3.5" />, who("reopened"))
    case "retargeted":
      return line(
        <ArrowRightLeft className="size-3.5" />,
        <>
          {e.author ? (
            <>
              <Who name={e.author} pull={pull} /> moved
            </>
          ) : (
            "Moved"
          )}{" "}
          this from <span className="font-mono text-xs">{e.old_oid}</span> to{" "}
          <span className="font-mono text-xs">{e.oid}</span>
          {!e.author && " on GitHub"}
        </>
      )
    case "merged":
      return line(
        <GitMerge className="size-3.5 text-ink" />,
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
          "mt-2 flex size-7 shrink-0 items-center justify-center rounded-full text-white",
          status.ok ? "bg-added" : "bg-removed"
        )}
      >
        {status.ok ? (
          <CheckCircle2 className="size-4" />
        ) : (
          <AlertTriangle className="size-4" />
        )}
      </span>
      <div className="min-w-0 flex-1 rounded-[9px] border">
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
          <div className="flex flex-wrap items-center gap-2 border-t border-hairline px-4 py-3">
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
      <Avatar name={state.user} className="mt-2 size-7 text-[11px]" />
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
                  <GitPullRequestClosed className="text-removed" />
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
