import { Check, Copy, Download } from "lucide-react"
import { useState } from "react"

import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { Input } from "@/components/ui/input"

export function cloneUrl(repo: string) {
  return `${location.origin}/${repo}.git`
}

export function CopyButton({ text }: { text: string }) {
  const [done, setDone] = useState(false)
  return (
    <Button
      size="icon"
      variant="outline"
      aria-label="Copy"
      onClick={async () => {
        await navigator.clipboard.writeText(text)
        setDone(true)
        setTimeout(() => setDone(false), 1500)
      }}
    >
      {done ? <Check /> : <Copy />}
    </Button>
  )
}

export function CloneButton({ repo }: { repo: string }) {
  const url = cloneUrl(repo)
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button size="sm">
          <Download />
          Clone
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-96 p-3">
        <DropdownMenuLabel className="px-0">Clone over HTTPS</DropdownMenuLabel>
        <div className="flex gap-2">
          <Input
            readOnly
            value={url}
            className="font-mono text-xs"
            onFocus={(e) => e.target.select()}
          />
          <CopyButton text={url} />
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
