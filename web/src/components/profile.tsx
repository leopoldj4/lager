import { Link } from "@tanstack/react-router"
import { BookLock, BookOpen } from "lucide-react"
import { useMemo } from "react"

import { useProfile, useRepos } from "@/lib/api"
import type { ActivityDay } from "@/lib/api"
import { href } from "@/lib/paths"
import { cn } from "@/lib/utils"
import { Avatar } from "@/components/repo/avatar"
import { Badge } from "@/components/ui/badge"
import { Skeleton } from "@/components/ui/skeleton"
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import { GithubMark } from "@/components/github-mark"
import { QueryError } from "@/components/query-error"
import { NotFound } from "@/routes/__root"
import { LiveError } from "@/lib/live"

const WEEKS = 53
/** Cell colours from none to most, on the accent. */
const LEVELS = [
  "bg-muted",
  "bg-blue-600/25",
  "bg-blue-600/45",
  "bg-blue-600/70",
  "bg-blue-600",
]
const dayName = new Intl.DateTimeFormat(undefined, {
  weekday: "short",
  month: "short",
  day: "numeric",
  year: "numeric",
})
const monthName = new Intl.DateTimeFormat(undefined, { month: "short" })

/** YYYY-MM-DD of a local date. */
function key(d: Date) {
  const m = String(d.getMonth() + 1).padStart(2, "0")
  const day = String(d.getDate()).padStart(2, "0")
  return `${d.getFullYear()}-${m}-${day}`
}

/** A user's page: their activity of the last year and their repositories. */
export function ProfilePage({ user }: { user: string }) {
  const profile = useProfile(user)
  const repos = useRepos()
  const own = repos.data?.repos.filter((r) => r.name.startsWith(`${user}/`))

  if (profile.error instanceof LiveError && profile.error.code === "not_found")
    return <NotFound />

  return (
    <div className="grid w-full grid-cols-1 gap-8">
      <div className="flex items-center gap-4">
        <Avatar name={user} className="size-14 text-lg" />
        <h1 className="text-2xl font-semibold tracking-tight">{user}</h1>
      </div>

      <section className="grid gap-3">
        <div className="flex flex-wrap items-baseline gap-x-4 gap-y-1">
          <h2 className="font-mono text-xs font-medium tracking-wider uppercase">
            Activity
          </h2>
          {profile.data && (
            <span className="font-mono text-xs text-muted-foreground">
              {(
                profile.data.totals.lager + profile.data.totals.github
              ).toLocaleString()}{" "}
              in the last year
            </span>
          )}
          {profile.data?.github && (
            <span className="ml-auto flex items-center gap-3 font-mono text-xs text-muted-foreground">
              <span>lager {profile.data.totals.lager.toLocaleString()}</span>
              <span className="flex items-center gap-1">
                <GithubMark />
                {profile.data.totals.github.toLocaleString()}
              </span>
            </span>
          )}
        </div>
        {profile.error ? (
          <QueryError error={profile.error} />
        ) : !profile.data ? (
          <Skeleton className="h-32" />
        ) : (
          <Graph days={profile.data.days} />
        )}
      </section>

      <section className="grid gap-3">
        <h2 className="font-mono text-xs font-medium tracking-wider uppercase">
          Repositories{" "}
          {own && (
            <span className="font-normal text-muted-foreground">
              {own.length}
            </span>
          )}
        </h2>
        {!own ? (
          <Skeleton className="h-14" />
        ) : own.length === 0 ? (
          <p className="text-sm text-muted-foreground">None to show.</p>
        ) : (
          <ul className="divide-y rounded-lg border">
            {own.map((r) => (
              <li key={r.name}>
                <Link
                  to="/$"
                  params={{ _splat: href(r.name) }}
                  className="flex items-center gap-3 px-4 py-2.5 text-sm hover:bg-muted/50"
                >
                  {r.anonymous_read ? (
                    <BookOpen className="size-4 shrink-0 text-muted-foreground" />
                  ) : (
                    <BookLock className="size-4 shrink-0 text-muted-foreground" />
                  )}
                  <span className="truncate font-medium">
                    {r.name.slice(user.length + 1)}
                  </span>
                  {r.mirror && (
                    <Badge
                      variant="outline"
                      title={`Mirror of github.com/${r.mirror}`}
                    >
                      <GithubMark />
                      mirror
                    </Badge>
                  )}
                </Link>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  )
}

/** Weeks as columns from Sunday to Saturday, the last one ending today. */
function Graph({ days }: { days: ActivityDay[] }) {
  const { weeks, months, max } = useMemo(() => {
    const byDate = new Map(days.map((d) => [d.date, d]))
    const today = new Date()
    today.setHours(12, 0, 0, 0)
    const start = new Date(today)
    start.setDate(start.getDate() - (WEEKS - 1) * 7 - today.getDay())
    const weeks: { date: Date; day?: ActivityDay; future: boolean }[][] = []
    const months: { week: number; label: string }[] = []
    let max = 0
    for (let w = 0; w < WEEKS; w++) {
      const week = []
      for (let d = 0; d < 7; d++) {
        const date = new Date(start)
        date.setDate(start.getDate() + w * 7 + d)
        const day = byDate.get(key(date))
        if (day) max = Math.max(max, day.lager + day.github)
        week.push({ date, day, future: date > today })
        if (date.getDate() === 1 && w < WEEKS - 1)
          months.push({ week: w, label: monthName.format(date) })
      }
      weeks.push(week)
    }
    return { weeks, months, max }
  }, [days])

  const level = (n: number) =>
    n === 0 ? 0 : Math.min(4, Math.max(1, Math.ceil((n / max) * 4)))

  return (
    <div className="grid gap-2">
      <div
        className="overflow-x-auto pb-1"
        // The newest weeks first in view on narrow screens.
        ref={(el) => {
          if (el) el.scrollLeft = el.scrollWidth
        }}
      >
        <div className="inline-grid grid-cols-[auto_1fr] gap-x-2">
          <div />
          <div className="relative h-4 font-mono text-[11px] text-muted-foreground">
            {months.map((m) => (
              <span
                key={`${m.week}-${m.label}`}
                className="absolute"
                style={{ left: `${m.week * 14}px` }}
              >
                {m.label}
              </span>
            ))}
          </div>
          <div className="grid grid-rows-7 gap-[3px] font-mono text-[11px] leading-[11px] text-muted-foreground">
            {["", "Mon", "", "Wed", "", "Fri", ""].map((d, i) => (
              <span key={i}>{d}</span>
            ))}
          </div>
          <div className="flex gap-[3px]">
            {weeks.map((week, w) => (
              <div key={w} className="grid grid-rows-7 gap-[3px]">
                {week.map(({ date, day, future }) => {
                  const total = day ? day.lager + day.github : 0
                  if (future)
                    return <div key={date.getTime()} className="size-[11px]" />
                  return (
                    <Tooltip key={date.getTime()}>
                      <TooltipTrigger asChild>
                        <div
                          className={cn(
                            "size-[11px] rounded-[2px] hover:ring-1 hover:ring-foreground/50",
                            LEVELS[level(total)]
                          )}
                        />
                      </TooltipTrigger>
                      <TooltipContent
                        sideOffset={4}
                        className="flex-col items-start gap-0.5"
                      >
                        <span>
                          <span className="font-medium">
                            {total === 0
                              ? "No contributions"
                              : `${total.toLocaleString()} ${total === 1 ? "contribution" : "contributions"}`}
                          </span>
                          <span className="opacity-70">
                            {" "}
                            · {dayName.format(date)}
                          </span>
                        </span>
                        {day && day.github > 0 && (
                          <span className="flex items-center gap-1 font-mono text-[11px] opacity-70">
                            lager {day.lager.toLocaleString()} ·
                            <GithubMark className="size-3" />
                            {day.github.toLocaleString()}
                          </span>
                        )}
                      </TooltipContent>
                    </Tooltip>
                  )
                })}
              </div>
            ))}
          </div>
        </div>
      </div>
      <div className="flex items-center gap-1 font-mono text-[11px] text-muted-foreground">
        <span className="mr-1 ml-auto">Less</span>
        {LEVELS.map((l) => (
          <span key={l} className={cn("size-[11px] rounded-[2px]", l)} />
        ))}
        <span className="ml-1">More</span>
      </div>
    </div>
  )
}
