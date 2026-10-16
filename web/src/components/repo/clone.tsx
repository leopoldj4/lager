import { useQuery } from "@tanstack/react-query"
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

/** The server's SSH address (`ssh.public_url`), or null without SSH. */
function useSshUrl() {
  return useQuery({
    queryKey: ["ssh-url"],
    queryFn: async () => {
      const r = await fetch("/-/api/ssh")
      if (!r.ok) return null
      const body: { url: string | null } = await r.json()
      return body.url
    },
    staleTime: Infinity,
  }).data
}

/** On port 22, the short form `git@host:org/app.git`. */
export function sshCloneUrl(base: string, repo: string) {
  const short = /^ssh:\/\/([^/:]+)$/.exec(base)
  return short ? `${short[1]}:${repo}.git` : `${base}/${repo}.git`
}

function UrlField({ url }: { url: string }) {
  return (
    <div className="flex gap-2">
      <Input
        readOnly
        value={url}
        className="font-mono text-xs"
        onFocus={(e) => e.target.select()}
      />
      <CopyButton text={url} />
    </div>
  )
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
  const ssh = useSshUrl()
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
        <UrlField url={url} />
        {ssh && (
          <>
            <DropdownMenuLabel className="mt-2 px-0">
              Clone over SSH
            </DropdownMenuLabel>
            <UrlField url={sshCloneUrl(ssh, repo)} />
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
