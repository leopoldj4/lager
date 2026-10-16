import { createContext, useContext, useEffect, useRef, useState } from "react"
import { useNavigate, useParams } from "@tanstack/react-router"
import { Command as CommandPrimitive } from "cmdk"
import { Dialog as DialogPrimitive } from "radix-ui"
import {
  BookLock,
  BookMarked,
  BookOpen,
  Copy,
  CornerDownLeft,
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
  Settings,
  Tag,
  Terminal,
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
import { cn } from "@/lib/utils"
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandItem,
  CommandList,
  CommandShortcut,
} from "@/components/ui/command"
import {
  Dialog,
  DialogDescription,
  DialogHeader,
  DialogPortal,
  DialogTitle,
} from "@/components/ui/dialog"
import { Separator } from "@/components/ui/separator"
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

/** What Enter does on an item, and what ⌘C copies, for the footer. */
type Hint = { action: string; copy?: { label: string; text: string } }

/** Hints by item value; items fill it in as they render. */
const HintsContext = createContext<Map<string, Hint>>(new Map())

/**
 * Search and jump: repositories, and in a repository its pull requests,
 * branches, and pages. Inside a repository the search starts there;
 * Backspace on an empty input widens it to everything. ⌘K or / opens it;
 * the footer tells what Enter and ⌘C do on the highlighted item.
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
  const [selected, setSelected] = useState("")
  const hints = useRef(new Map<string, Hint>()).current
  const hint = hints.get(selected)

  useEffect(() => {
    const down = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault()
        onOpenChange(!open)
      } else if (e.key === "/" && !open && !editable(e.target)) {
        e.preventDefault()
        onOpenChange(true)
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
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogPortal>
        <DialogPrimitive.Content className="fixed top-[15%] left-1/2 z-50 grid w-full max-w-[calc(100%-2rem)] -translate-x-1/2 rounded-xl bg-popover bg-clip-padding p-2 pb-11 text-popover-foreground shadow-2xl ring-4 ring-neutral-200/80 duration-100 outline-none sm:max-w-xl dark:bg-neutral-900 dark:ring-neutral-800 data-open:animate-in data-open:fade-in-0 data-open:zoom-in-95 data-closed:animate-out data-closed:fade-out-0 data-closed:zoom-out-95">
          <DialogHeader className="sr-only">
            <DialogTitle>Search</DialogTitle>
            <DialogDescription>
              Jump to a repository, pull request, branch, or page.
            </DialogDescription>
          </DialogHeader>
          <Command
            filter={match}
            value={selected}
            onValueChange={setSelected}
            className="rounded-none! bg-transparent p-0"
          >
            <div className="flex h-9 items-center gap-2 rounded-md border border-input bg-input/50 px-3">
              <SearchIcon className="size-4 shrink-0 opacity-50" />
              {scope && (
                <button
                  type="button"
                  tabIndex={-1}
                  onClick={() => setScoped(false)}
                  className="flex shrink-0 items-center gap-1 rounded-sm bg-muted px-1.5 py-0.5 text-xs font-medium text-foreground hover:bg-muted/70"
                  aria-label={`Search everywhere instead of ${scope}`}
                >
                  {scope}
                  <X className="size-3" />
                </button>
              )}
              <CommandPrimitive.Input
                data-slot="command-input"
                value={query}
                onValueChange={setQuery}
                onKeyDown={(e) => {
                  if (e.key === "Backspace" && query === "" && scope) {
                    setScoped(false)
                  }
                  const input = e.currentTarget
                  if (
                    e.key.toLowerCase() === "c" &&
                    (e.metaKey || e.ctrlKey) &&
                    input.selectionStart === input.selectionEnd &&
                    hint?.copy
                  ) {
                    e.preventDefault()
                    const text = hint.copy.text
                    run(() => navigator.clipboard.writeText(text))
                  }
                }}
                placeholder={
                  scope ? "Search this repository…" : "Search or jump to…"
                }
                className="h-full w-full min-w-0 flex-1 bg-transparent text-sm outline-hidden placeholder:text-muted-foreground"
              />
            </div>
            <CommandList className="max-h-80 min-h-80 scroll-pt-2 scroll-pb-1.5">
              <CommandEmpty className="py-12 text-muted-foreground">
                No results found.
              </CommandEmpty>
              <HintsContext value={hints}>
                {scope && <RepoGroups repo={scope} go={go} run={run} />}
                <Actions run={run} onCreateRepo={onCreateRepo} />
                <RepoList go={go} />
                <Account run={run} />
              </HintsContext>
            </CommandList>
          </Command>
          <div className="absolute inset-x-0 bottom-0 z-20 flex h-10 items-center gap-2 rounded-b-xl border-t border-t-neutral-100 bg-neutral-50 px-4 text-xs font-medium text-muted-foreground dark:border-t-neutral-700 dark:bg-neutral-800">
            {hint && (
              <div className="flex items-center gap-2">
                <MenuKbd>
                  <CornerDownLeft />
                </MenuKbd>
                {hint.action}
              </div>
            )}
            {hint?.copy && (
              <>
                <Separator
                  orientation="vertical"
                  className="h-4! self-center!"
                />
                <div className="flex min-w-0 items-center gap-1">
                  <MenuKbd>
                    <ModKey />
                  </MenuKbd>
                  <MenuKbd>C</MenuKbd>
                  <span className="truncate">{hint.copy.label}</span>
                </div>
              </>
            )}
          </div>
        </DialogPrimitive.Content>
      </DialogPortal>
    </Dialog>
  )
}

function editable(target: EventTarget | null) {
  return (
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    target instanceof HTMLSelectElement ||
    (target instanceof HTMLElement && target.isContentEditable)
  )
}

function MenuKbd({ className, ...props }: React.ComponentProps<"kbd">) {
  return (
    <kbd
      className={cn(
        "pointer-events-none flex h-5 min-w-5 items-center justify-center gap-1 rounded border bg-background px-1 font-sans text-[0.7rem] font-medium text-muted-foreground select-none [&_svg:not([class*='size-'])]:size-3",
        className
      )}
      {...props}
    />
  )
}

function MenuGroup({
  className,
  ...props
}: React.ComponentProps<typeof CommandGroup>) {
  return (
    <CommandGroup
      className={cn(
        "p-0! **:[[cmdk-group-heading]]:scroll-mt-16 **:[[cmdk-group-heading]]:p-3! **:[[cmdk-group-heading]]:pb-1!",
        className
      )}
      {...props}
    />
  )
}

function MenuItem({
  hint,
  className,
  ...props
}: React.ComponentProps<typeof CommandItem> & { value: string; hint: Hint }) {
  useContext(HintsContext).set(props.value, hint)
  return (
    <CommandItem
      className={cn(
        "h-9 rounded-md border border-transparent px-3! font-medium data-selected:border-input data-selected:bg-input/50",
        className
      )}
      {...props}
    />
  )
}

/** The page's address to share, from its splat. */
function pageUrl(splat: string) {
  return `${location.origin}/${splat}`
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
    <MenuGroup heading="Actions">
      <MenuItem
        value="action new repository"
        hint={{ action: "Create repository" }}
        keywords={["new repository", "create repo"]}
        onSelect={() => run(onCreateRepo)}
      >
        <Plus />
        New repository
      </MenuItem>
    </MenuGroup>
  )
}

function RepoGroups({ repo, go, run }: { repo: string; go: Go; run: Run }) {
  const refs = useRefs(repo).data
  // Mirrors have no pull requests.
  const hasPulls = !!refs && !refs.mirror
  const pulls = usePulls(repo, false, hasPulls).data?.pages.flatMap(
    (p) => p.pulls
  )
  return (
    <>
      {hasPulls && pulls && pulls.length > 0 && (
        <MenuGroup heading="Pull requests">
          {pulls.map((p) => (
            <MenuItem
              key={p.number}
              value={`pull ${p.number}`}
              hint={{
                action: "Open pull request",
                copy: {
                  label: "Copy link",
                  text: pageUrl(href(repo, "pull", String(p.number))),
                },
              }}
              keywords={[`#${p.number}`, p.title, p.source, p.author]}
              onSelect={() => go(href(repo, "pull", String(p.number)))}
            >
              <PullIcon state={p.state} />
              <span className="truncate">{p.title}</span>
              <CommandShortcut className="tracking-normal">
                #{p.number}
              </CommandShortcut>
            </MenuItem>
          ))}
        </MenuGroup>
      )}
      <Pages repo={repo} refs={refs} hasPulls={hasPulls} go={go} run={run} />
      {refs && refs.branches.length > 0 && (
        <MenuGroup heading="Branches">
          {[...refs.branches]
            .sort((a, b) => b.time - a.time)
            .map((b) => (
              <MenuItem
                key={b.name}
                value={`branch ${b.name}`}
                hint={{
                  action: "Browse branch",
                  copy: { label: "Copy branch name", text: b.name },
                }}
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
              </MenuItem>
            ))}
        </MenuGroup>
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
  hasPulls,
  go,
  run,
}: {
  repo: string
  refs: Refs | undefined
  hasPulls: boolean
  go: Go
  run: Run
}) {
  const user = useLive()?.user
  return (
    <MenuGroup heading="Go to">
      {pages
        .filter((p) => hasPulls || p.view !== "pulls")
        .map((p) => (
          <MenuItem
            key={p.view}
            value={`page ${p.label}`}
            hint={{
              action: "Go to page",
              copy: {
                label: "Copy link",
                text: pageUrl(
                  p.view === "overview" ? href(repo) : href(repo, p.view)
                ),
              },
            }}
            keywords={[p.label]}
            onSelect={() =>
              go(p.view === "overview" ? href(repo) : href(repo, p.view))
            }
          >
            <p.icon />
            {p.label}
          </MenuItem>
        ))}
      {user && hasPulls && refs && refs.branches.length > 1 && (
        <MenuItem
          value="action new pull request"
          hint={{ action: "Compare branches" }}
          keywords={["new pull request", "open", "compare"]}
          onSelect={() => go(newPullHref(repo, refs))}
        >
          <Plus />
          New pull request
        </MenuItem>
      )}
      <MenuItem
        value="action copy clone url"
        hint={{ action: "Copy clone URL" }}
        keywords={["copy clone url", "git"]}
        onSelect={() =>
          run(() => navigator.clipboard.writeText(cloneUrl(repo)))
        }
      >
        <Copy />
        Copy clone URL
      </MenuItem>
    </MenuGroup>
  )
}

function RepoList({ go }: { go: Go }) {
  const repos = useRepos().data?.repos
  if (!repos || repos.length === 0) return null
  return (
    <MenuGroup heading="Repositories">
      {repos.map((r) => (
        <MenuItem
          key={r.name}
          value={`repo ${r.name}`}
          hint={{
            action: "Open repository",
            copy: { label: "Copy clone URL", text: cloneUrl(r.name) },
          }}
          keywords={[r.name]}
          onSelect={() => go(href(r.name))}
        >
          {r.anonymous_read ? <BookOpen /> : <BookLock />}
          <span className="truncate">{r.name}</span>
        </MenuItem>
      ))}
    </MenuGroup>
  )
}

function Account({ run }: { run: Run }) {
  const state = useLive()
  const navigate = useNavigate()
  if (!state?.ready) return null
  return (
    <MenuGroup heading="Account">
      {state.user ? (
        <>
          <MenuItem
            value="account settings github"
            hint={{ action: "Open settings" }}
            keywords={["settings", "preferences", "github", "mirror"]}
            onSelect={() => run(() => navigate({ to: "/-/settings/github" }))}
          >
            <Settings />
            Settings
          </MenuItem>
          <MenuItem
            value="account passkeys"
            hint={{ action: "Open settings" }}
            keywords={["passkeys"]}
            onSelect={() => run(() => navigate({ to: "/-/settings/passkeys" }))}
          >
            <KeyRound />
            Passkeys
          </MenuItem>
          <MenuItem
            value="account access tokens"
            hint={{ action: "Open settings" }}
            keywords={["tokens", "personal access token", "pat"]}
            onSelect={() => run(() => navigate({ to: "/-/settings/tokens" }))}
          >
            <Key />
            Access tokens
          </MenuItem>
          <MenuItem
            value="account ssh keys"
            hint={{ action: "Open settings" }}
            keywords={["ssh", "public key", "git over ssh"]}
            onSelect={() => run(() => navigate({ to: "/-/settings/ssh-keys" }))}
          >
            <Terminal />
            SSH keys
          </MenuItem>
          <MenuItem
            value="account sign out"
            hint={{ action: "Sign out" }}
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
          </MenuItem>
        </>
      ) : (
        <MenuItem
          value="account sign in"
          hint={{ action: "Sign in" }}
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
        </MenuItem>
      )}
    </MenuGroup>
  )
}
