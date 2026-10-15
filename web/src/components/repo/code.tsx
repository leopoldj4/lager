import { useEffect, useRef, useState } from "react"
import { Link } from "@tanstack/react-router"
import { FileDown, History, PanelLeft } from "lucide-react"
import {
  useDefaultLayout,
  type PanelImperativeHandle,
} from "react-resizable-panels"

import type { Refs } from "@/lib/api"
import { href, rawHref } from "@/lib/paths"
import { cn } from "@/lib/utils"
import { useIsMobile } from "@/hooks/use-mobile"
import { Button } from "@/components/ui/button"
import { Kbd, KbdGroup } from "@/components/ui/kbd"
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from "@/components/ui/resizable"
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet"
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import { ModKey } from "@/components/omnibar"
import { BlobView } from "./blob"
import { FileTree } from "./file-tree"
import { PathCrumbs } from "./path-crumbs"
import { RevPicker } from "./rev-picker"
import { TreeView } from "./tree"

/**
 * The code of a revision: the file tree beside the directory or file the
 * address names. On phones the tree opens in a sheet.
 */
export function CodeView({
  repo,
  rev,
  path,
  refs,
  view,
}: {
  repo: string
  rev: string
  path: string
  refs: Refs
  view: "tree" | "blob"
}) {
  const mobile = useIsMobile()
  const [sheet, setSheet] = useState(false)
  const panel = useRef<PanelImperativeHandle>(null)
  // A hidden tree leaves no divider and no gap beside the preview.
  const [collapsed, setCollapsed] = useState(false)
  const layout = useDefaultLayout({ id: "worktree-code" })
  const toggle = () => {
    if (mobile) return setSheet((s) => !s)
    const p = panel.current
    if (p?.isCollapsed()) p.expand()
    else p?.collapse()
  }
  // The latest toggle, for the shortcut, which is bound once.
  const toggleRef = useRef(toggle)
  toggleRef.current = toggle
  useEffect(() => {
    const down = (e: KeyboardEvent) => {
      if (e.key === "\\" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault()
        toggleRef.current()
      }
    }
    document.addEventListener("keydown", down)
    return () => document.removeEventListener("keydown", down)
  }, [])

  const preview =
    view === "blob" ? (
      <BlobView repo={repo} rev={rev} path={path} />
    ) : (
      <TreeView repo={repo} rev={rev} path={path} />
    )
  const tree = (onNavigate?: () => void) => (
    <FileTree repo={repo} rev={rev} path={path} onNavigate={onNavigate} />
  )

  return (
    <div className="grid grid-cols-1 gap-4">
      <div className="flex flex-wrap items-center gap-3">
        <Tooltip>
          <TooltipTrigger asChild>
            <Button
              variant="ghost"
              size="icon-sm"
              onClick={toggle}
              aria-label="Show or hide files"
            >
              <PanelLeft />
            </Button>
          </TooltipTrigger>
          <TooltipContent>
            Files{" "}
            <KbdGroup>
              <Kbd>
                <ModKey />
              </Kbd>
              <Kbd>\</Kbd>
            </KbdGroup>
          </TooltipContent>
        </Tooltip>
        <RevPicker repo={repo} view={view} rev={rev} path={path} refs={refs} />
        <PathCrumbs repo={repo} rev={rev} path={path} />
        <div className="ml-auto flex gap-1">
          {view === "blob" && (
            <Button asChild variant="ghost" size="sm">
              <a href={rawHref(repo, rev, path)}>
                <FileDown />
                Raw
              </a>
            </Button>
          )}
          <Button asChild variant="ghost" size="sm">
            <Link to="/$" params={{ _splat: href(repo, "commits", rev, path) }}>
              <History />
              History
            </Link>
          </Button>
        </div>
      </div>
      {mobile ? (
        <>
          {preview}
          <Sheet open={sheet} onOpenChange={setSheet}>
            <SheetContent side="left" className="w-72 gap-0 p-0">
              <SheetHeader className="border-b">
                <SheetTitle>Files</SheetTitle>
              </SheetHeader>
              <div className="overflow-y-auto px-2">
                {tree(() => setSheet(false))}
              </div>
            </SheetContent>
          </Sheet>
        </>
      ) : (
        // Both sides scroll on their own, so the tree stays in view. The
        // group sets its own height to 100%, so the wrapper sizes it.
        <div className="h-[calc(100svh-12.5rem)] min-h-96">
          <ResizablePanelGroup
            id="worktree-code"
            defaultLayout={layout.defaultLayout}
            onLayoutChanged={layout.onLayoutChanged}
          >
            <ResizablePanel
              id="files"
              panelRef={panel}
              onResize={(size) => setCollapsed(size.inPixels === 0)}
              collapsible
              defaultSize={280}
              minSize={180}
              maxSize="40"
              className="pr-2"
            >
              {tree()}
            </ResizablePanel>
            <ResizableHandle className={cn(collapsed && "bg-transparent")} />
            <ResizablePanel
              id="preview"
              minSize="40"
              className={cn(!collapsed && "pl-4")}
            >
              {preview}
            </ResizablePanel>
          </ResizablePanelGroup>
        </div>
      )}
    </div>
  )
}
