import { useNavigate } from "@tanstack/react-router"
import { GitBranch, Tag } from "lucide-react"

import type { Refs } from "@/lib/wire"
import { href, type View } from "@/lib/paths"
import { short } from "@/lib/format"
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"

/** Switches the branch or tag of the page, keeping the path. */
export function RevPicker({
  repo,
  view,
  rev,
  path,
  refs,
}: {
  repo: string
  view: View
  rev: string
  path: string
  refs: Refs
}) {
  const navigate = useNavigate()
  const known =
    refs.branches.some((b) => b.name === rev) ||
    refs.tags.some((t) => t.name === rev)
  return (
    <Select
      value={rev}
      onValueChange={(r) =>
        navigate({ to: "/$", params: { _splat: href(repo, view, r, path) } })
      }
    >
      <SelectTrigger
        size="sm"
        aria-label="Branch or tag"
        className="h-5.5! max-w-56 min-w-0 shrink gap-1.25 rounded-[5px] px-2 font-mono text-[11px] text-body shadow-none [&_svg]:size-3! [&_svg]:text-muted-foreground"
      >
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {!known && <SelectItem value={rev}>{short(rev)}</SelectItem>}
        <SelectGroup>
          <SelectLabel>Branches</SelectLabel>
          {refs.branches.map((b) => (
            <SelectItem key={b.name} value={b.name} className="font-mono">
              <GitBranch />
              {b.name}
            </SelectItem>
          ))}
        </SelectGroup>
        {refs.tags.length > 0 && (
          <SelectGroup>
            <SelectLabel>Tags</SelectLabel>
            {refs.tags.map((t) => (
              <SelectItem key={t.name} value={t.name} className="font-mono">
                <Tag />
                {t.name}
              </SelectItem>
            ))}
          </SelectGroup>
        )}
      </SelectContent>
    </Select>
  )
}
