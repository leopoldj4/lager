import { Link } from "@tanstack/react-router"

import type { Person } from "@/lib/api"
import { cn } from "@/lib/utils"

/** A commit author's name, linked to the profile of the user whose email it is. */
export function AuthorName({
  author,
  className,
}: {
  author: Person
  className?: string
}) {
  if (!author.user) return <span className={className}>{author.name}</span>
  return (
    <Link
      to="/$"
      params={{ _splat: author.user }}
      title={author.user}
      className={cn(className, "hover:text-foreground hover:underline")}
    >
      {author.name}
    </Link>
  )
}
