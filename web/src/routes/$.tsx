import { createFileRoute } from "@tanstack/react-router"

import { parse } from "@/lib/paths"
import { NotFound } from "./__root"
import { RepoPage } from "@/components/repo/repo-page"

export const Route = createFileRoute("/$")({ component: Page })

function Page() {
  const { _splat } = Route.useParams()
  const loc = parse(_splat ?? "")
  if (!loc) return <NotFound />
  // A new repository starts from a fresh page state.
  return <RepoPage key={loc.repo} loc={loc} />
}
