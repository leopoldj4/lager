import { useEffect, useState } from "react"
import { useNavigate, useParams } from "@tanstack/react-router"
import { Command as CommandPrimitive } from "cmdk"
import {
  BookLock,
  BookMarked,
  BookOpen,
  Copy,
  GitBranch,
  GitCommitHorizontal,
  GitPullRequest,
  Key,
  KeyRound,
  LayoutDashboard,
  LogIn,
  LogOut,
  Plus,
  SearchIcon,
  Tag,
  X,
} from "lucide-react"

import {
  signOut,
  useLive,
  usePulls,
  useRefs,
  useRepos,
  type Refs,
} from "@/lib/api"
import { href, parse, type View } from "@/lib/paths"
import {
  Command,
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandItem,
  CommandList,
  CommandShortcut,
} from "@/components/ui/command"
import { InputGroup, InputGroupAddon } from "@/components/ui/input-group"
import { Kbd, KbdGroup } from "@/components/ui/kbd"
import { cloneUrl } from "@/components/repo/clone"
import { PullIcon, newPullHref } from "@/components/repo/pulls"

/** The repository of the current page, if any. */
export function useCurrentRepo() {
  const { _splat } = useParams({ strict: false })
  return _splat ? parse(_splat)?.repo : undefined
}

/** ⌘ on Apple keyboards, Ctrl elsewhere. */
export function ModKey() {
  const [mac, setMac] = useState(false)
  useEffect(() => setMac(/Mac|iPhone|iPad/.test(navigator.platform)), [])
  return <>{mac ? "⌘" : "Ctrl"}</>
}

export function OmnibarShortcut({ className }: { className?: string }) {
  return (
    <KbdGroup className={className}>
      <Kbd>
        <ModKey />
      </Kbd>
      <Kbd>K</Kbd>
    </KbdGroup>
  )
}

/**
 * Every word of the search appears in an item's keywords; items whose
 * words start with the search come first. cmdk's default matches any
 * letters in order, which lists nearly everything for short searches.
 */
function match(value: string, search: string, keywords?: string[]) {
  const text = (keywords?.length ? keywords : [value]).join(" ").toLowerCase()
  const terms = search.toLowerCase().split(/\s+/).filter(Boolean)
  if (!terms.every((t) => text.includes(t))) return 0
  return text.split(/[\s/_.-]+/).some((w) => w.startsWith(terms[0] ?? ""))
    ? 1
    : 0.5
}

/**
 * Search and jump: repositories, and in a repository its pull requests,
 * branches, and pages. Inside a repository the search starts there;
 * Backspace on an empty input widens it to everything.
 */
export function Omnibar({
  open,
  onOpenChange,
  onCreateRepo,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  onCreateRepo: () => void
}) {
  const repo = useCurrentRepo()
  const navigate = useNavigate()
  const [query, setQuery] = useState("")
  const [scoped, setScoped] = useState(true)

  useEffect(() => {
    const down = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault()
        onOpenChange(!open)
      }
    }
    document.addEventListener("keydown", down)
    return () => document.removeEventListener("keydown", down)
  }, [open, onOpenChange])

  useEffect(() => {
    if (!open) return
    setQuery("")
    setScoped(true)
  }, [open])

  const scope = scoped ? repo : undefined
  const run = (action: () => void) => {
    onOpenChange(false)
    action()
  }
  const go = (splat: string) =>
    run(() => navigate({ to: "/$", params: { _splat: splat } }))

  return (
    <CommandDialog
      open={open}
      onOpenChange={onOpenChange}
      title="Search"
      description="Jump to a repository, pull request, branch, or page."
      className="sm:max-w-xl"
    >
      <Command filter={match}>
        <div className="p-1 pb-0">
          <InputGroup className="h-9 border-input/30 bg-input/30 shadow-none">
            <InputGroupAddon>
              <SearchIcon className="opacity-50" />
              {scope && (
                <button
                  type="button"
                  onClick={() => setScoped(false)}
                  className="flex items-center gap-1 rounded-md bg-muted px-1.5 py-0.5 text-xs text-foreground hover:bg-muted/70"
                  aria-label={`Search everywhere instead of ${scope}`}
                >
                  {scope}
                  <X className="size-3" />
                </button>
              )}
            </InputGroupAddon>
            <CommandPrimitive.Input
              value={query}
              onValueChange={setQuery}
              onKeyDown={(e) => {
                if (e.key === "Backspace" && query === "" && scope) {
                  setScoped(false)
                }
              }}
              placeholder={
                scope ? "Search this repository…" : "Search or jump to…"
              }
              className="h-full w-full flex-1 bg-transparent text-sm outline-hidden"
            />
          </InputGroup>
        </div>
        <CommandList className="max-h-[min(28rem,60vh)]">
          <CommandEmpty>Nothing found.</CommandEmpty>
          {scope && <RepoGroups repo={scope} go={go} run={run} />}
          <Actions run={run} onCreateRepo={onCreateRepo} />
          <RepoList go={go} />
          <Account run={run} />
        </CommandList>
      </Command>
    </CommandDialog>
  )
}

type Go = (splat: string) => void
type Run = (action: () => void) => void

function Actions({
  run,
  onCreateRepo,
}: {
  run: Run
  onCreateRepo: () => void
}) {
  const user = useLive()?.user
  if (!user) return null
  return (
    <CommandGroup heading="Actions">
      <CommandItem
        value="action new repository"
        keywords={["new repository", "create repo"]}
        onSelect={() => run(onCreateRepo)}
      >
        <Plus />
        New repository
      </CommandItem>
    </CommandGroup>
  )
}

function RepoGroups({ repo, go, run }: { repo: string; go: Go; run: Run }) {
  const refs = useRefs(repo).data
  const pulls = usePulls(repo, false).data?.pages.flatMap((p) => p.pulls)
  return (
    <>
      {pulls && pulls.length > 0 && (
        <CommandGroup heading="Pull requests">
          {pulls.map((p) => (
            <CommandItem
              key={p.number}
              value={`pull ${p.number}`}
              keywords={[`#${p.number}`, p.title, p.source, p.author]}
              onSelect={() => go(href(repo, "pull", String(p.number)))}
            >
              <PullIcon state={p.state} />
              <span className="truncate">{p.title}</span>
              <CommandShortcut className="tracking-normal">
                #{p.number}
              </CommandShortcut>
            </CommandItem>
          ))}
        </CommandGroup>
      )}
      <Pages repo={repo} refs={refs} go={go} run={run} />
      {refs && refs.branches.length > 0 && (
        <CommandGroup heading="Branches">
          {[...refs.branches]
            .sort((a, b) => b.time - a.time)
            .map((b) => (
              <CommandItem
                key={b.name}
                value={`branch ${b.name}`}
                keywords={[b.name]}
                onSelect={() => go(href(repo, "tree", b.name))}
              >
                <GitBranch />
                <span className="truncate font-mono text-xs">{b.name}</span>
                {b.name === refs.head && (
                  <CommandShortcut className="tracking-normal">
                    default
                  </CommandShortcut>
                )}
              </CommandItem>
            ))}
        </CommandGroup>
      )}
    </>
  )
}

const pages: { view: View; label: string; icon: typeof Tag }[] = [
  { view: "overview", label: "Overview", icon: LayoutDashboard },
  { view: "pulls", label: "Pull requests", icon: GitPullRequest },
  { view: "tree", label: "Code", icon: BookMarked },
  { view: "commits", label: "Commits", icon: GitCommitHorizontal },
  { view: "branches", label: "Branches", icon: GitBranch },
  { view: "tags", label: "Tags", icon: Tag },
]

function Pages({
  repo,
  refs,
  go,
  run,
}: {
  repo: string
  refs: Refs | undefined
  go: Go
  run: Run
}) {
  const user = useLive()?.user
  return (
    <CommandGroup heading="Go to">
      {pages.map((p) => (
        <CommandItem
          key={p.view}
          value={`page ${p.label}`}
          keywords={[p.label]}
          onSelect={() =>
            go(p.view === "overview" ? href(repo) : href(repo, p.view))
          }
        >
          <p.icon />
          {p.label}
        </CommandItem>
      ))}
      {user && refs && refs.branches.length > 1 && (
        <CommandItem
          value="action new pull request"
          keywords={["new pull request", "open", "compare"]}
          onSelect={() => go(newPullHref(repo, refs))}
        >
          <Plus />
          New pull request
        </CommandItem>
      )}
      <CommandItem
        value="action copy clone url"
        keywords={["copy clone url", "git"]}
        onSelect={() =>
          run(() => navigator.clipboard.writeText(cloneUrl(repo)))
        }
      >
        <Copy />
        Copy clone URL
      </CommandItem>
    </CommandGroup>
  )
}

function RepoList({ go }: { go: Go }) {
  const repos = useRepos().data?.repos
  if (!repos || repos.length === 0) return null
  return (
    <CommandGroup heading="Repositories">
      {repos.map((r) => (
        <CommandItem
          key={r.name}
          value={`repo ${r.name}`}
          keywords={[r.name]}
          onSelect={() => go(href(r.name))}
        >
          {r.anonymous_read ? <BookOpen /> : <BookLock />}
          <span className="truncate">{r.name}</span>
        </CommandItem>
      ))}
    </CommandGroup>
  )
}

function Account({ run }: { run: Run }) {
  const state = useLive()
  const navigate = useNavigate()
  if (!state?.ready) return null
  return (
    <CommandGroup heading="Account">
      {state.user ? (
        <>
          <CommandItem
            value="account passkeys"
            keywords={["passkeys"]}
            onSelect={() => run(() => navigate({ to: "/-/passkeys" }))}
          >
            <KeyRound />
            Passkeys
          </CommandItem>
          <CommandItem
            value="account access tokens"
            keywords={["tokens", "personal access token", "pat"]}
            onSelect={() => run(() => navigate({ to: "/-/tokens" }))}
          >
            <Key />
            Access tokens
          </CommandItem>
          <CommandItem
            value="account sign out"
            keywords={["sign out", "log out"]}
            onSelect={() =>
              run(async () => {
                await signOut()
                navigate({ to: "/" })
              })
            }
          >
            <LogOut />
            Sign out
          </CommandItem>
        </>
      ) : (
        <CommandItem
          value="account sign in"
          keywords={["sign in", "log in"]}
          onSelect={() =>
            run(() =>
              navigate({
                to: "/-/sign-in",
                search: { next: location.pathname },
              })
            )
          }
        >
          <LogIn />
          Sign in
        </CommandItem>
      )}
    </CommandGroup>
  )
}
