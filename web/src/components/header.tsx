import { Link, useNavigate } from "@tanstack/react-router"
import { GitBranch, LogOut, User } from "lucide-react"

import { signOut, useLive } from "@/lib/api"
import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"

export function Header() {
  const state = useLive()
  const navigate = useNavigate()
  return (
    <header className="border-b">
      <div className="mx-auto flex h-14 max-w-6xl items-center gap-4 px-4">
        <Link to="/" className="flex items-center gap-2 font-semibold">
          <GitBranch className="size-5" />
          worktree
        </Link>
        <div className="flex-1" />
        <LiveDot />
        {state?.user ? (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="sm">
                <User />
                {state.user}
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuLabel>Signed in as {state.user}</DropdownMenuLabel>
              <DropdownMenuSeparator />
              <DropdownMenuItem
                onSelect={async () => {
                  await signOut()
                  navigate({ to: "/" })
                }}
              >
                <LogOut />
                Sign out
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        ) : state?.ready ? (
          <Button asChild size="sm" variant="outline">
            <Link to="/-/sign-in" search={{ next: location.pathname }}>
              Sign in
            </Link>
          </Button>
        ) : null}
      </div>
    </header>
  )
}

/** Whether pushes arrive: green while the live connection is open. */
function LiveDot() {
  const state = useLive()
  const status = state?.status ?? "connecting"
  const label = {
    open: `Live${state?.server ? ` · server ${state.server}` : ""}`,
    connecting: "Connecting…",
    offline: "Offline, reconnecting…",
  }[status]
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span className="flex items-center gap-2 text-xs text-muted-foreground">
          <span className="flex size-2">
            <span
              className={cn(
                "relative inline-flex size-2 rounded-full",
                status === "open" && "bg-emerald-500",
                status === "connecting" && "bg-amber-500",
                status === "offline" && "bg-red-500"
              )}
            />
          </span>
          <span className="hidden sm:inline">
            {status === "open"
              ? "Live"
              : status === "connecting"
                ? "Connecting"
                : "Offline"}
          </span>
        </span>
      </TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  )
}
