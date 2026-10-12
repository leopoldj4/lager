import {
  HeadContent,
  Link,
  Outlet,
  Scripts,
  createRootRoute,
} from "@tanstack/react-router"
import { QueryClientProvider } from "@tanstack/react-query"
import { useEffect } from "react"

import appCss from "../styles.css?url"
import { queryClient } from "@/lib/api"
import { live } from "@/lib/live"
import { TooltipProvider } from "@/components/ui/tooltip"
import { Header } from "@/components/header"

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
  return (
    <QueryClientProvider client={queryClient}>
      <TooltipProvider>
        <div className="flex min-h-svh flex-col">
          <Header />
          <main className="mx-auto w-full max-w-6xl flex-1 px-4 py-6">
            <Outlet />
          </main>
        </div>
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
