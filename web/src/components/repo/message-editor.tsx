import { useState } from "react"

import { cn } from "@/lib/utils"
import { Textarea } from "@/components/ui/textarea"
import { MessageView } from "./markdown"

/** A Markdown text with a preview, for descriptions and comments. */
export function MessageEditor({
  repo,
  rev,
  value,
  onChange,
  label,
  placeholder,
  className,
}: {
  repo: string
  /** Relative links in the preview start at the root of this branch. */
  rev: string
  value: string
  onChange: (value: string) => void
  label: string
  placeholder: string
  className?: string
}) {
  const [preview, setPreview] = useState(false)
  return (
    <div className="grid grid-cols-1 gap-1.5">
      <div role="tablist" aria-label={label} className="flex gap-1">
        {[false, true].map((p) => (
          <button
            key={String(p)}
            type="button"
            role="tab"
            aria-selected={preview === p}
            onClick={() => setPreview(p)}
            className={cn(
              "rounded-md px-2.5 py-1 text-sm",
              preview === p
                ? "bg-muted font-medium"
                : "text-muted-foreground hover:text-foreground"
            )}
          >
            {p ? "Preview" : "Write"}
          </button>
        ))}
        <span className="ml-auto self-center text-xs text-muted-foreground">
          Markdown
        </span>
      </div>
      {preview ? (
        <div
          className={cn("rounded-md border px-3 py-2", className)}
          role="tabpanel"
        >
          {value.trim() ? (
            <MessageView repo={repo} rev={rev} text={value} />
          ) : (
            <p className="text-sm text-muted-foreground italic">
              Nothing to preview.
            </p>
          )}
        </div>
      ) : (
        <Textarea
          aria-label={label}
          placeholder={placeholder}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className={className}
        />
      )}
    </div>
  )
}
