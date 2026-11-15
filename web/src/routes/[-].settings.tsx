import {
  createFileRoute,
  Link,
  Outlet,
  useLocation,
} from "@tanstack/react-router"

import { useLive } from "@/lib/query"
import { cn } from "@/lib/utils"
import { Crumbs, PageBody, PageHeader, PageTitle } from "@/components/frame"

export const Route = createFileRoute("/-/settings")({ component: Settings })

const GROUPS = [
  {
    title: "Account",
    sections: [{ to: "/-/settings/emails", title: "Emails" }],
  },
  {
    title: "Integrations",
    sections: [{ to: "/-/settings/github", title: "GitHub" }],
  },
  {
    title: "Security",
    sections: [
      { to: "/-/settings/passkeys", title: "Passkeys" },
      { to: "/-/settings/tokens", title: "Access tokens" },
      { to: "/-/settings/ssh-keys", title: "SSH keys" },
    ],
  },
] as const

function Settings() {
  const state = useLive()
  const pathname = useLocation({ select: (l) => l.pathname })
  // With the query, so a verification link survives signing in.
  const here = useLocation({ select: (l) => l.href })
  const section = GROUPS.flatMap(
    (g): readonly { to: string; title: string }[] => g.sections
  ).find((s) => s.to === pathname)
  return (
    <>
      <PageHeader>
        <Crumbs
          items={[
            { label: "Settings" },
            ...(section ? [{ label: section.title }] : []),
          ]}
        />
      </PageHeader>
      <PageBody>
        {!state?.ready ? (
          <p className="text-sm text-muted-foreground">Connecting…</p>
        ) : !state.user ? (
          <p className="text-sm">
            <Link
              to="/-/sign-in"
              search={{ next: here }}
              className="font-medium text-primary hover:underline"
            >
              Sign in
            </Link>{" "}
            to change your settings.
          </p>
        ) : (
          <div className="grid gap-8">
            <PageTitle>Settings</PageTitle>
            <div className="flex min-w-0 flex-col gap-8 lg:flex-row lg:gap-12">
              <aside className="lg:w-48 lg:shrink-0">
                <nav className="flex gap-1 overflow-x-auto lg:flex-col lg:gap-5">
                  {GROUPS.map((g) => (
                    <div
                      key={g.title}
                      className="flex gap-1 lg:flex-col lg:gap-px"
                    >
                      <h2 className="hidden px-2.5 pb-1.5 label-caps lg:block">
                        {g.title}
                      </h2>
                      {g.sections.map((s) => (
                        <Link
                          key={s.to}
                          to={s.to}
                          className={cn(
                            "flex h-7.5 shrink-0 items-center rounded-md px-2.5 text-[13px] font-medium whitespace-nowrap",
                            pathname === s.to
                              ? "bg-tint text-ink"
                              : "text-body hover:bg-muted"
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
        )}
      </PageBody>
    </>
  )
}
