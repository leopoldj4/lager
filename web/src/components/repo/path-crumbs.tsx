import { Link } from "@tanstack/react-router"
import { Fragment } from "react"

import { href } from "@/lib/paths"

/** repo / dir / dir / file, each part a link to its directory. */
export function PathCrumbs({
  repo,
  rev,
  path,
}: {
  repo: string
  rev: string
  path: string
}) {
  const parts = path.split("/").filter(Boolean)
  const name = repo.split("/").pop()
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-1 font-mono text-sm">
      <Link
        to="/$"
        params={{ _splat: href(repo, "tree", rev) }}
        className="font-semibold hover:underline"
      >
        {name}
      </Link>
      {parts.map((p, i) => (
        <Fragment key={i}>
          <span className="text-muted-foreground">/</span>
          {i === parts.length - 1 ? (
            <span className="font-semibold">{p}</span>
          ) : (
            <Link
              to="/$"
              params={{
                _splat: href(
                  repo,
                  "tree",
                  rev,
                  parts.slice(0, i + 1).join("/")
                ),
              }}
              className="hover:underline"
            >
              {p}
            </Link>
          )}
        </Fragment>
      ))}
    </div>
  )
}
