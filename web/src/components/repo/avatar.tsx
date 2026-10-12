import { cn } from "@/lib/utils"

/** Initials on a color derived from the name; no external images. */
export function Avatar({
  name,
  className,
}: {
  name: string
  className?: string
}) {
  let h = 0
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) % 360
  const initials = name
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((w) => w[0]?.toUpperCase())
    .join("")
  return (
    <span
      className={cn(
        "inline-flex size-6 shrink-0 items-center justify-center rounded-full text-[10px] font-semibold text-white",
        className
      )}
      style={{ backgroundColor: `oklch(0.6 0.12 ${h})` }}
      aria-hidden
    >
      {initials || "?"}
    </span>
  )
}
