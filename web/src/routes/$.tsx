import { createFileRoute } from "@tanstack/react-router"

import { parse } from "@/lib/paths"
import { NotFound } from "./__root"
import { RepoPage } from "@/components/repo/repo-page"
import { ProfilePage } from "@/components/profile"

export const Route = createFileRoute("/$")({ component: Page })

function Page() {
  const { _splat } = Route.useParams()
  // A repository name has at least two segments; one is a user.
  const segs = (_splat ?? "").split("/").filter(Boolean)
  if (segs.length === 1 && segs[0] !== "-")
    return <ProfilePage key={segs[0]} user={segs[0]} />
  const loc = parse(_splat ?? "")
  if (!loc) return <NotFound />
  // A new repository starts from a fresh page state.
  return <RepoPage key={loc.repo} loc={loc} />
}
