import {
  HeadContent,
  Link,
  Outlet,
  Scripts,
  createRootRoute,
  useLocation,
} from "@tanstack/react-router"
import { QueryClientProvider } from "@tanstack/react-query"
import { useEffect, useState, useSyncExternalStore } from "react"

import appCss from "../styles.css?url"
import logo from "../assets/lager.svg?url"
import { queryClient, useLive } from "@/lib/query"
import { live } from "@/lib/live"
import { TooltipProvider } from "@/components/ui/tooltip"
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar"
import { AppSidebar } from "@/components/app-sidebar"
import { PageBody, PageHeader } from "@/components/frame"
import { Omnibar } from "@/components/omnibar"
import { CreateRepoDialog } from "@/components/create-repo-dialog"
import { Landing } from "@/components/landing"

// Follows the system color scheme before the first paint.
const theme = `(() => {
  const m = matchMedia("(prefers-color-scheme: dark)")
  const set = () => document.documentElement.classList.toggle("dark", m.matches)
  set()
  m.addEventListener("change", set)
})()`

// The landing page (docs/decisions/0034-landing-page.md) is for lager.sh and
// for development; other installations list their public repositories at /.
const LANDING_HOSTS = "(^|\\.)lager\\.sh$"
// Set while a user is signed in, so that the next visit knows which page to
// draw before the live connection answers.
const SIGNED_IN = "lager:signed-in"

function landingHere() {
  return (
    location.pathname === "/" &&
    (import.meta.env.DEV || new RegExp(LANDING_HOSTS).test(location.hostname))
  )
}

// The shell holds the app frame; hide it before the first paint where the
// landing page replaces it.
const landing = `(() => {
  if (
    location.pathname === "/" &&
    (${import.meta.env.DEV} || new RegExp("${LANDING_HOSTS}").test(location.hostname)) &&
    !localStorage.getItem("${SIGNED_IN}")
  )
    document.documentElement.classList.add("landing")
})()`

export const Route = createRootRoute({
  head: () => ({
    meta: [
      { charSet: "utf-8" },
      { name: "viewport", content: "width=device-width, initial-scale=1" },
      { title: "lager" },
      {
        name: "description",
        content:
          "A forge for agents and people. Instant, realtime, and in sync with GitHub.",
      },
    ],
    links: [
      { rel: "stylesheet", href: appCss },
      { rel: "icon", type: "image/svg+xml", href: logo },
    ],
  }),
  component: Root,
  notFoundComponent: NotFound,
  shellComponent: Shell,
})

function Shell({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: theme }} />
        <script dangerouslySetInnerHTML={{ __html: landing }} />
        <HeadContent />
      </head>
      <body>
        {children}
        <Scripts />
      </body>
    </html>
  )
}

function Root() {
  useEffect(() => live.start(), [])
  const [search, setSearch] = useState(false)
  const [createRepo, setCreateRepo] = useState(false)
  const openSearch = () => setSearch(true)

  const state = useLive()
  useEffect(() => {
    if (!state?.ready) return
    if (state.user) localStorage.setItem(SIGNED_IN, "1")
    else localStorage.removeItem(SIGNED_IN)
  }, [state?.ready, state?.user])
  // False while hydrating, where the page must match the shell.
  const hydrated = useSyncExternalStore(
    () => () => {},
    () => true,
    () => false
  )
  const pathname = useLocation({ select: (l) => l.pathname })
  const showLanding =
    hydrated &&
    pathname === "/" &&
    landingHere() &&
    (state?.ready ? !state.user : !localStorage.getItem(SIGNED_IN))
  useEffect(() => {
    if (hydrated) document.documentElement.classList.remove("landing")
  }, [hydrated])

  if (showLanding)
    return (
      <QueryClientProvider client={queryClient}>
        <Landing />
      </QueryClientProvider>
    )
  return (
    <QueryClientProvider client={queryClient}>
      <TooltipProvider>
        {/* Always open on desktop; phones still open it as a sheet. */}
        <SidebarProvider
          open
          onOpenChange={() => {}}
          className="bg-sidebar md:h-svh md:overflow-hidden"
        >
          <AppSidebar
            onSearch={openSearch}
            onCreateRepo={() => setCreateRepo(true)}
          />
          {/* Each page draws its own header above its content. On desktop the
              main area is a rounded panel inset from the window, which
              scrolls on its own. */}
          <SidebarInset
            data-scroll-restoration-id="main"
            className="min-w-0 md:m-(--frame-gap) md:ml-0 md:h-(--frame-height) md:overflow-y-auto md:rounded-(--frame-radius) md:shadow-xs"
          >
            <Outlet />
          </SidebarInset>
          <Omnibar
            open={search}
            onOpenChange={setSearch}
            onCreateRepo={() => setCreateRepo(true)}
          />
          <CreateRepoDialog
            open={createRepo}
            onOpenChange={setCreateRepo}
            trigger={false}
          />
        </SidebarProvider>
      </TooltipProvider>
    </QueryClientProvider>
  )
}

export function NotFound() {
  return (
    <>
      <PageHeader>
        <span className="text-[13px] font-medium text-ink">Not found</span>
      </PageHeader>
      <PageBody className="py-24 text-center">
        <h1 className="font-display text-[34px] leading-9 text-ink">
          Not found
        </h1>
        <p className="mt-2 text-sm text-muted-foreground">
          This page does not exist, or you cannot see it.
        </p>
        <Link
          to="/"
          className="mt-6 inline-block text-sm font-medium text-primary hover:underline"
        >
          All repositories
        </Link>
      </PageBody>
    </>
  )
}
