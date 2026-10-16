import {
  createFileRoute,
  Link,
  Outlet,
  useLocation,
} from "@tanstack/react-router"

import { useLive } from "@/lib/api"
import { cn } from "@/lib/utils"
import { buttonVariants } from "@/components/ui/button"
import { Separator } from "@/components/ui/separator"

export const Route = createFileRoute("/-/settings")({ component: Settings })

const GROUPS = [
  {
    title: "Integrations",
    sections: [{ to: "/-/settings/github", title: "GitHub" }],
  },
  {
    title: "Security",
    sections: [
      { to: "/-/settings/passkeys", title: "Passkeys" },
      { to: "/-/settings/tokens", title: "Access tokens" },
    ],
  },
] as const

function Settings() {
  const state = useLive()
  const pathname = useLocation({ select: (l) => l.pathname })
  if (!state?.ready) return <p>Connecting…</p>
  if (!state.user)
    return (
      <p>
        <Link to="/-/sign-in" search={{ next: pathname }} className="underline">
          Sign in
        </Link>{" "}
        to change your settings.
      </p>
    )
  return (
    <div className="mx-auto w-full max-w-5xl">
      <h1 className="text-2xl font-semibold tracking-tight">Settings</h1>
      <Separator className="my-6" />
      <div className="flex flex-col gap-8 lg:flex-row lg:gap-12">
        <aside className="-mx-1 lg:w-48 lg:shrink-0">
          <nav className="flex gap-1 overflow-x-auto lg:flex-col lg:gap-4">
            {GROUPS.map((g) => (
              <div key={g.title} className="flex gap-1 lg:flex-col">
                <h2 className="hidden px-2.5 py-1 text-xs font-medium text-muted-foreground lg:block">
                  {g.title}
                </h2>
                {g.sections.map((s) => (
                  <Link
                    key={s.to}
                    to={s.to}
                    className={cn(
                      buttonVariants({ variant: "ghost" }),
                      "justify-start",
                      pathname === s.to
                        ? "bg-muted hover:bg-muted"
                        : "hover:bg-transparent hover:underline"
                    )}
                  >
                    {s.title}
                  </Link>
                ))}
              </div>
            ))}
          </nav>
        </aside>
        <div className="min-w-0 flex-1 lg:max-w-2xl">
          <Outlet />
        </div>
      </div>
    </div>
  )
}
