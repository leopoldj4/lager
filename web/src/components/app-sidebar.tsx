import { useEffect, useState, type ReactNode } from "react"
import { Link, useLocation, useNavigate } from "@tanstack/react-router"
import {
  ChevronsUpDown,
  LibraryBig,
  LogIn,
  LogOut,
  Plus,
  SearchIcon,
  Settings,
  User,
} from "lucide-react"

import { usePullCounts } from "@/lib/pulls"
import { useLive } from "@/lib/query"
import { useRepos } from "@/lib/repos"
import type { RepoSummary } from "@/lib/wire"
import { signOut } from "@/lib/session"
import { href, type Location, type View } from "@/lib/paths"
import { cn } from "@/lib/utils"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarHeader,
  useSidebar,
} from "@/components/ui/sidebar"
import { Skeleton } from "@/components/ui/skeleton"
import { Avatar, RepoDot } from "@/components/repo/avatar"
import { LagerMark } from "@/components/lager-mark"
import { GithubMark } from "@/components/github-mark"
import { ModKey, useCurrentLocation } from "@/components/omnibar"

/** Repositories per group before "All repositories". */
const SHOWN = 8

/** A row of the sidebar; the selected one is a white card with a hairline ring. */
const item = (active: boolean) =>
  cn(
    "flex h-7.5 w-full min-w-0 shrink-0 items-center gap-2.5 rounded-md px-2 text-left text-[13px] font-medium text-body outline-none hover:bg-ink/[0.04] focus-visible:ring-2 focus-visible:ring-ring/50 [&>svg]:size-[15px] [&>svg]:shrink-0 [&>svg]:text-muted-foreground",
    active &&
      "bg-sidebar-accent text-ink shadow-[0_0_0_1px_var(--stage-off),0_1px_2px_#0000000a] hover:bg-sidebar-accent"
  )

/** A small muted count at the end of a row. */
function Count({ children }: { children: ReactNode }) {
  return (
    <span className="ml-auto shrink-0 text-[11px] leading-3.5 font-medium text-label">
      {children}
    </span>
  )
}

export function AppSidebar({
  onSearch,
  onCreateRepo,
}: {
  onSearch: () => void
  onCreateRepo: () => void
}) {
  const user = useLive()?.user
  const repos = useRepos()
  const loc = useCurrentLocation()
  const pathname = useLocation({ select: (l) => l.pathname })
  const close = useCloseOnMobile()

  const own = user
    ? repos.data?.repos.filter((r) => r.name.startsWith(`${user}/`))
    : []
  const others = repos.data?.repos.filter((r) => !own?.includes(r))

  return (
    <Sidebar className="group-data-[side=left]:border-r-0">
      <SidebarHeader className="gap-4.5 px-3 pt-4 pb-0">
        <div className="flex items-center justify-between py-1 pr-0.5 pl-1.5">
          <Link to="/" onClick={close} className="flex items-center gap-2.25">
            <span className="flex size-6.5 items-center justify-center rounded-md bg-[#F3ECE2]">
              <LagerMark className="size-5" />
            </span>
            <span className="font-display text-[19px] leading-6 text-ink">
              lager
            </span>
          </Link>
          {user && (
            <button
              type="button"
              onClick={() => {
                close()
                onCreateRepo()
              }}
              className="flex size-7 items-center justify-center rounded-md text-ink hover:bg-ink/[0.05]"
              aria-label="New repository"
              title="New repository"
            >
              <Plus className="size-3.5" strokeWidth={2.4} />
            </button>
          )}
        </div>
        <button
          type="button"
          onClick={() => {
            close()
            onSearch()
          }}
          className="flex h-9.5 w-full shrink-0 items-center gap-2.5 rounded-[10px] bg-background pr-2.5 pl-3 text-left shadow-[0_0_0_1px_var(--stage-off),0_1px_2px_#0000000a] outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
        >
          <SearchIcon className="size-[15px] text-label" />
          <span className="flex-1 truncate text-[13px] text-label">
            Jump to anything
          </span>
          <span className="flex gap-0.75 text-[11px] leading-3.5 font-medium text-muted-foreground">
            <kbd className="rounded-sm bg-muted px-1.25 py-px font-sans">
              <ModKey />
            </kbd>
            <kbd className="rounded-sm bg-muted px-1.25 py-px font-sans">K</kbd>
          </span>
        </button>
      </SidebarHeader>
      <SidebarContent className="gap-4.5 px-3 pt-4.5">
        <nav className="flex flex-col gap-px">
          <Link to="/" onClick={close} className={item(pathname === "/")}>
            <LibraryBig />
            Repositories
            {repos.data && <Count>{repos.data.repos.length}</Count>}
          </Link>
          {user && (
            <Link
              to="/-/settings"
              onClick={close}
              className={item(pathname.startsWith("/-/settings"))}
            >
              <Settings />
              Settings
            </Link>
          )}
        </nav>
        {!repos.data ? (
          <div className="flex flex-col gap-2 px-2">
            {[0, 1, 2].map((i) => (
              <Skeleton key={i} className="h-5" />
            ))}
          </div>
        ) : (
          <>
            {own && own.length > 0 && (
              <RepoGroup
                label="Yours"
                repos={own}
                loc={loc}
                strip={`${user}/`}
              />
            )}
            {others && others.length > 0 && (
              <RepoGroup
                label={user ? "Others" : "Public"}
                repos={others}
                loc={loc}
              />
            )}
          </>
        )}
      </SidebarContent>
      <SidebarFooter className="gap-1 px-3 pt-2 pb-3">
        <LiveStatus />
        <UserMenu />
      </SidebarFooter>
    </Sidebar>
  )
}

/** On phones the sidebar is a sheet; following a link closes it. */
function useCloseOnMobile() {
  const { isMobile, setOpenMobile } = useSidebar()
  return () => {
    if (isMobile) setOpenMobile(false)
  }
}

function RepoGroup({
  label,
  repos,
  loc,
  strip,
}: {
  label: string
  repos: RepoSummary[]
  loc: Location | null
  /** A prefix not to repeat, such as the user's own name. */
  strip?: string
}) {
  const close = useCloseOnMobile()
  // The current repository stays visible even past the limit.
  const shown = repos.filter((r, i) => i < SHOWN || r.name === loc?.repo)
  return (
    <section className="flex flex-col gap-px">
      <h2 className="px-2 pb-1.5 label-caps">{label}</h2>
      {shown.map((r) => (
        <RepoItem
          key={r.name}
          repo={r}
          loc={r.name === loc?.repo ? loc : null}
          label={strip ? r.name.slice(strip.length) : r.name}
        />
      ))}
      {repos.length > shown.length && (
        <Link
          to="/"
          onClick={close}
          className={cn(item(false), "pl-[33px] text-muted-foreground")}
        >
          All {repos.length}
        </Link>
      )}
    </section>
  )
}

function RepoItem({
  repo,
  loc,
  label,
}: {
  repo: RepoSummary
  /** The page, when it belongs to this repository. */
  loc: Location | null
  label: string
}) {
  const close = useCloseOnMobile()
  const open = usePullCounts(repo.name).data?.open
  return (
    <>
      <Link
        to="/$"
        params={{ _splat: href(repo.name) }}
        onClick={close}
        title={repo.name}
        className={cn(item(false), loc && "font-semibold text-ink")}
      >
        <span className="flex w-[15px] shrink-0 justify-center">
          {repo.mirror ? (
            <GithubMark className="size-3 text-label" />
          ) : (
            <RepoDot name={repo.name} />
          )}
        </span>
        <span className="truncate">{label}</span>
        {open ? <Count>{open}</Count> : null}
      </Link>
      {loc && <RepoSections loc={loc} open={open} />}
    </>
  )
}

/** The pages of the open repository, on an indented rail. */
function RepoSections({ loc, open }: { loc: Location; open?: number }) {
  const close = useCloseOnMobile()
  // Only the owner changes the settings (the server checks it too).
  const owner = useLive()?.user === loc.repo.split("/")[0]
  const active: View =
    loc.view === "blob"
      ? "tree"
      : loc.view === "commit" || loc.view === "branches"
        ? "commits"
        : loc.view === "pull" || loc.view === "compare"
          ? "pulls"
          : loc.view
  const sections: { view: View; label: string; count?: number }[] = [
    { view: "overview", label: "Overview" },
    { view: "tree", label: "Code" },
    { view: "pulls", label: "Pull requests", count: open || undefined },
    { view: "commits", label: "Commits" },
  ]
  if (owner) sections.push({ view: "settings", label: "Settings" })
  return (
    <div className="pt-0.5 pb-1.5 pl-3.75">
      <div className="flex flex-col gap-px border-l border-stage-off pl-2.5">
        {sections.map((s) => (
          <Link
            key={s.view}
            to="/$"
            params={{
              _splat:
                s.view === "overview" ? href(loc.repo) : href(loc.repo, s.view),
            }}
            onClick={close}
            className={cn(
              item(active === s.view),
              "h-7 px-2.5",
              active !== s.view && "text-[#55555D] dark:text-body"
            )}
          >
            {s.label}
            {s.count !== undefined && <Count>{s.count}</Count>}
          </Link>
        ))}
      </div>
    </div>
  )
}

function UserMenu() {
  const state = useLive()
  const navigate = useNavigate()
  const close = useCloseOnMobile()
  if (!state?.ready) return null
  if (!state.user) {
    return (
      <Link
        to="/-/sign-in"
        search={{ next: location.pathname }}
        onClick={close}
        className={item(false)}
      >
        <LogIn />
        Sign in
      </Link>
    )
  }
  return (
    <DropdownMenu>
      <DropdownMenuTrigger className={cn(item(false), "group h-9")}>
        <Avatar name={state.user} className="size-5.5" />
        <span className="flex-1 truncate">{state.user}</span>
        <ChevronsUpDown className="opacity-0 group-hover:opacity-100 group-data-[state=open]:opacity-100" />
      </DropdownMenuTrigger>
      <DropdownMenuContent side="top" align="start" className="w-56">
        <DropdownMenuLabel>Signed in as {state.user}</DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuItem asChild>
          <Link to="/$" params={{ _splat: state.user }} onClick={close}>
            <User />
            Profile
          </Link>
        </DropdownMenuItem>
        <DropdownMenuItem asChild>
          <Link to="/-/settings" onClick={close}>
            <Settings />
            Settings
          </Link>
        </DropdownMenuItem>
        <DropdownMenuItem
          onSelect={async () => {
            close()
            await signOut()
            navigate({ to: "/" })
          }}
        >
          <LogOut />
          Sign out
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

/** How long the connection may be down before the status appears. */
const LIVE_GRACE_MS = 5000

/** Shown only once the live connection has been down a while, so pushes may be missed. */
function LiveStatus() {
  const state = useLive()
  const status = state?.status ?? "connecting"
  const down = status !== "open"
  const [late, setLate] = useState(false)
  useEffect(() => {
    setLate(false)
    if (!down) return
    const timer = setTimeout(() => setLate(true), LIVE_GRACE_MS)
    return () => clearTimeout(timer)
  }, [down])
  if (status === "open" || !late) return null
  return (
    <div className="flex items-center gap-2 px-2 text-xs text-muted-foreground">
      <span
        className={cn(
          "size-2 rounded-full",
          status === "connecting" && "bg-label",
          status === "offline" && "bg-removed"
        )}
      />
      {status === "connecting" ? "Connecting…" : "Offline, reconnecting…"}
    </div>
  )
}
