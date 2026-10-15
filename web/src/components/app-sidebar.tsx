import { useEffect, useState } from "react"
import { Link, useLocation, useNavigate } from "@tanstack/react-router"
import {
  BookLock,
  BookOpen,
  ChevronsUpDown,
  GitBranch,
  House,
  KeyRound,
  LogIn,
  LogOut,
  SearchIcon,
} from "lucide-react"

import {
  signOut,
  useLive,
  usePullCounts,
  useRepos,
  type RepoSummary,
} from "@/lib/api"
import { href } from "@/lib/paths"
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
  SidebarGroup,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSkeleton,
  SidebarRail,
  SidebarTrigger,
  useSidebar,
} from "@/components/ui/sidebar"
import { Avatar } from "@/components/repo/avatar"
import { Button } from "@/components/ui/button"
import { OmnibarShortcut, useCurrentRepo } from "@/components/omnibar"

/** Repositories per group before "All repositories". */
const SHOWN = 8

export function AppSidebar({ onSearch }: { onSearch: () => void }) {
  const user = useLive()?.user
  const repos = useRepos()
  const current = useCurrentRepo()
  const pathname = useLocation({ select: (l) => l.pathname })
  const close = useCloseOnMobile()

  const own = user
    ? repos.data?.repos.filter((r) => r.name.startsWith(`${user}/`))
    : []
  const others = repos.data?.repos.filter((r) => !own?.includes(r))

  return (
    <Sidebar>
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton asChild className="font-semibold">
              <Link to="/" onClick={close}>
                <GitBranch />
                worktree
              </Link>
            </SidebarMenuButton>
          </SidebarMenuItem>
          <SidebarMenuItem>
            <SidebarMenuButton
              variant="outline"
              className="text-muted-foreground"
              onClick={onSearch}
            >
              <SearchIcon />
              <span>Search or jump to…</span>
              <OmnibarShortcut className="ml-auto" />
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>
      <SidebarContent>
        <SidebarGroup>
          <SidebarMenu>
            <SidebarMenuItem>
              <SidebarMenuButton asChild isActive={pathname === "/"}>
                <Link to="/" onClick={close}>
                  <House />
                  Repositories
                </Link>
              </SidebarMenuButton>
            </SidebarMenuItem>
          </SidebarMenu>
        </SidebarGroup>
        {!repos.data ? (
          <SidebarGroup>
            <SidebarMenu>
              {[0, 1, 2].map((i) => (
                <SidebarMenuItem key={i}>
                  <SidebarMenuSkeleton showIcon />
                </SidebarMenuItem>
              ))}
            </SidebarMenu>
          </SidebarGroup>
        ) : (
          <>
            {own && own.length > 0 && (
              <RepoGroup
                label="Your repositories"
                repos={own}
                current={current}
                strip={`${user}/`}
              />
            )}
            {others && others.length > 0 && (
              <RepoGroup
                label={user ? "Other repositories" : "Public repositories"}
                repos={others}
                current={current}
              />
            )}
          </>
        )}
      </SidebarContent>
      <SidebarFooter>
        <LiveStatus />
        <UserMenu />
      </SidebarFooter>
      <SidebarRail />
    </Sidebar>
  )
}

/** On phones, where the sidebar is hidden: open it, go home, or search. */
export function MobileHeader({ onSearch }: { onSearch: () => void }) {
  return (
    <header className="sticky top-0 z-10 flex h-12 items-center gap-2 border-b bg-background px-3 md:hidden">
      <SidebarTrigger />
      <Link to="/" className="flex items-center gap-2 font-semibold">
        <GitBranch className="size-4" />
        worktree
      </Link>
      <Button
        variant="ghost"
        size="icon"
        className="ml-auto"
        aria-label="Search"
        onClick={onSearch}
      >
        <SearchIcon />
      </Button>
    </header>
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
  current,
  strip,
}: {
  label: string
  repos: RepoSummary[]
  current: string | undefined
  /** A prefix not to repeat, such as the user's own name. */
  strip?: string
}) {
  const close = useCloseOnMobile()
  // The current repository stays visible even past the limit.
  const shown = repos.filter((r, i) => i < SHOWN || r.name === current)
  return (
    <SidebarGroup>
      <SidebarGroupLabel>{label}</SidebarGroupLabel>
      <SidebarMenu>
        {shown.map((r) => (
          <RepoItem
            key={r.name}
            repo={r}
            active={r.name === current}
            label={strip ? r.name.slice(strip.length) : r.name}
          />
        ))}
        {repos.length > shown.length && (
          <SidebarMenuItem>
            <SidebarMenuButton
              asChild
              size="sm"
              className="text-muted-foreground"
            >
              <Link to="/" onClick={close}>
                All {repos.length} repositories
              </Link>
            </SidebarMenuButton>
          </SidebarMenuItem>
        )}
      </SidebarMenu>
    </SidebarGroup>
  )
}

function RepoItem({
  repo,
  active,
  label,
}: {
  repo: RepoSummary
  active: boolean
  label: string
}) {
  const close = useCloseOnMobile()
  const open = usePullCounts(repo.name).data?.open
  const Icon = repo.anonymous_read ? BookOpen : BookLock
  return (
    <SidebarMenuItem>
      <SidebarMenuButton asChild isActive={active} tooltip={repo.name}>
        <Link to="/$" params={{ _splat: href(repo.name) }} onClick={close}>
          <Icon />
          <span>{label}</span>
        </Link>
      </SidebarMenuButton>
      {!!open && (
        <SidebarMenuBadge title={`${open} open pull requests`}>
          {open}
        </SidebarMenuBadge>
      )}
    </SidebarMenuItem>
  )
}

function UserMenu() {
  const state = useLive()
  const navigate = useNavigate()
  const close = useCloseOnMobile()
  if (!state?.ready) return null
  if (!state.user) {
    return (
      <SidebarMenu>
        <SidebarMenuItem>
          <SidebarMenuButton asChild>
            <Link
              to="/-/sign-in"
              search={{ next: location.pathname }}
              onClick={close}
            >
              <LogIn />
              Sign in
            </Link>
          </SidebarMenuButton>
        </SidebarMenuItem>
      </SidebarMenu>
    )
  }
  return (
    <SidebarMenu>
      <SidebarMenuItem>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <SidebarMenuButton size="lg">
              <Avatar name={state.user} className="size-7" />
              <span className="truncate font-medium">{state.user}</span>
              <ChevronsUpDown className="ml-auto" />
            </SidebarMenuButton>
          </DropdownMenuTrigger>
          <DropdownMenuContent side="top" align="start" className="w-56">
            <DropdownMenuLabel>Signed in as {state.user}</DropdownMenuLabel>
            <DropdownMenuSeparator />
            <DropdownMenuItem asChild>
              <Link to="/-/passkeys" onClick={close}>
                <KeyRound />
                Passkeys
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
      </SidebarMenuItem>
    </SidebarMenu>
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
          status === "connecting" && "bg-amber-500",
          status === "offline" && "bg-red-500"
        )}
      />
      {status === "connecting" ? "Connecting…" : "Offline, reconnecting…"}
    </div>
  )
}
