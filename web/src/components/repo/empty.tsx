import { cloneUrl } from "./clone"

export function EmptyRepo({ repo }: { repo: string }) {
  const url = cloneUrl(repo)
  return (
    <div className="rounded-lg border p-6">
      <h2 className="font-semibold">This repository is empty</h2>
      <p className="mt-1 text-sm text-muted-foreground">
        Push a branch to it. This page shows the push as soon as it lands.
      </p>
      <pre className="mt-4 overflow-x-auto rounded-md bg-muted p-4 font-mono text-sm">
        {`git remote add origin ${url}\ngit push -u origin main`}
      </pre>
    </div>
  )
}
