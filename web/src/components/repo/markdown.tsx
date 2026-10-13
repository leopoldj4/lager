// Markdown files and READMEs, rendered with react-markdown and GitHub's
// extensions. HTML in the file is left out, so a repository cannot run
// scripts on this site. Relative links open the repository's pages, and
// relative images load from the server's raw files.

import { Link } from "@tanstack/react-router"
import { useMemo } from "react"
import Markdown, { type Components } from "react-markdown"
import remarkGfm from "remark-gfm"

import { href, rawHref, resolvePath } from "@/lib/paths"

const plugins = [remarkGfm]

/** Matches addresses with a scheme ("https:", "mailto:") or a host ("//"). */
const absolute = /^([a-z][a-z\d+.-]*:|\/\/)/i

export function MarkdownView({
  repo,
  rev,
  path,
  text,
}: {
  repo: string
  rev: string
  /** The file's path, which relative links start from. */
  path: string
  text: string
}) {
  const components = useMemo<Components>(() => {
    // The repository path of a relative address, and its fragment.
    const local = (url: string | undefined) => {
      if (!url || url.startsWith("#") || absolute.test(url)) return null
      const [rest, hash] = url.split("#", 2)
      const target = resolvePath(path, rest.split("?")[0])
      if (target === null) return null
      return { target, dir: rest.endsWith("/"), hash }
    }
    return {
      a: ({ node: _, href: url, children, ...props }) => {
        const l = local(url)
        if (!l)
          return (
            <a href={url} {...props}>
              {children}
            </a>
          )
        // A directory without a trailing "/" opens as a file first; the
        // file page then moves to the directory.
        const view = l.dir || l.target === "" ? "tree" : "blob"
        return (
          <Link
            to="/$"
            params={{ _splat: href(repo, view, rev, l.target) }}
            hash={l.hash}
            {...props}
          >
            {children}
          </Link>
        )
      },
      img: ({ node: _, src, ...props }) => {
        const l = typeof src === "string" ? local(src) : null
        return <img src={l ? rawHref(repo, rev, l.target) : src} {...props} />
      },
    }
  }, [repo, rev, path])
  return (
    <div className="prose prose-sm max-w-none p-6 break-words dark:prose-invert">
      <Markdown remarkPlugins={plugins} components={components} skipHtml>
        {text}
      </Markdown>
    </div>
  )
}

export function isMarkdown(path: string) {
  return /\.(md|markdown|mdown|mkd)$/i.test(path)
}
