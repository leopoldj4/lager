import {
  HeadContent,
  Link,
  Outlet,
  Scripts,
  createRootRoute,
} from "@tanstack/react-router"
import { QueryClientProvider } from "@tanstack/react-query"
import { useEffect, useState } from "react"

import appCss from "../styles.css?url"
import { queryClient } from "@/lib/api"
import { live } from "@/lib/live"
import { TooltipProvider } from "@/components/ui/tooltip"
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar"
import { AppSidebar, MobileHeader } from "@/components/app-sidebar"
import { Omnibar } from "@/components/omnibar"
import { CreateRepoDialog } from "@/components/create-repo-dialog"

// Follows the system color scheme before the first paint.
const theme = `(() => {
  const m = matchMedia("(prefers-color-scheme: dark)")
  const set = () => document.documentElement.classList.toggle("dark", m.matches)
  set()
  m.addEventListener("change", set)
})()`

export const Route = createRootRoute({
  head: () => ({
    meta: [
      { charSet: "utf-8" },
      { name: "viewport", content: "width=device-width, initial-scale=1" },
      { title: "worktree" },
    ],
    links: [{ rel: "stylesheet", href: appCss }],
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
  return (
    <QueryClientProvider client={queryClient}>
      <TooltipProvider>
        <SidebarProvider>
          <AppSidebar onSearch={openSearch} />
          <SidebarInset>
            <MobileHeader onSearch={openSearch} />
            <div className="w-full flex-1 px-4 py-6">
              <Outlet />
            </div>
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
    <div className="py-24 text-center">
      <h1 className="text-2xl font-semibold">Not found</h1>
      <p className="mt-2 text-muted-foreground">
        This page does not exist, or you cannot see it.
      </p>
      <Link to="/" className="mt-6 inline-block text-sm underline">
        All repositories
      </Link>
    </div>
  )
}
