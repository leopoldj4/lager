// Markdown files and READMEs, rendered with react-markdown and GitHub's
// extensions. HTML in the file is left out, so a repository cannot run
// scripts on this site. Relative links open the repository's pages, and
// relative images load from the server's raw files.
//
// Descriptions and comments of pull requests are Markdown too
// (MessageView). They keep the HTML that GitHub keeps (details, images,
// tables), cleaned with GitHub's rules, and link #123, @login, and
// commit IDs.

import { Link } from "@tanstack/react-router"
import type { Parent, PhrasingContent, Root as Mdast } from "mdast"
import { useMemo } from "react"
import Markdown, { type Components, type Options } from "react-markdown"
import rehypeRaw from "rehype-raw"
import rehypeSanitize from "rehype-sanitize"
import remarkGfm from "remark-gfm"

import { href, rawHref, resolvePath } from "@/lib/paths"
import { cn } from "@/lib/utils"

/** Matches addresses with a scheme ("https:", "mailto:") or a host ("//"). */
const absolute = /^([a-z][a-z\d+.-]*:|\/\/)/i

/** Links and images of a Markdown text in the file at `path`. */
function useComponents(repo: string, rev: string, path: string) {
  return useMemo<Components>(() => {
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
        // A page of this site, such as a reference to a pull request.
        if (url?.startsWith("/") && url.includes("/-/")) {
          const [path, hash] = url.split("#", 2)
          return (
            <Link
              to="/$"
              params={{ _splat: path.slice(1) }}
              hash={hash}
              {...props}
            >
              {children}
            </Link>
          )
        }
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
}

const filePlugins = [remarkGfm]

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
  const components = useComponents(repo, rev, path)
  return (
    <div className="prose prose-sm max-w-none p-6 break-words dark:prose-invert">
      <Markdown remarkPlugins={filePlugins} components={components} skipHtml>
        {text}
      </Markdown>
    </div>
  )
}

export function isMarkdown(path: string) {
  return /\.(md|markdown|mdown|mkd)$/i.test(path)
}

/**
 * A description or comment. Relative links start at the root of `rev`.
 * `github`: the mirrored GitHub repository, whose users @login names.
 */
export function MessageView({
  repo,
  rev,
  github,
  text,
  className,
}: {
  repo: string
  rev: string
  github?: string | null
  text: string
  className?: string
}) {
  const components = useComponents(repo, rev, "")
  const remarkPlugins = useMemo(
    () => [remarkGfm, references(repo, github ?? null)],
    [repo, github]
  )
  return (
    <div
      className={cn(
        "prose prose-sm max-w-none break-words dark:prose-invert prose-code:before:content-none prose-code:after:content-none prose-pre:whitespace-pre-wrap",
        className
      )}
    >
      <Markdown
        remarkPlugins={remarkPlugins}
        rehypePlugins={messageRehype}
        components={components}
      >
        {text}
      </Markdown>
    </div>
  )
}

// HTML is parsed and then cleaned with GitHub's rules: no scripts, styles,
// event handlers, or forms.
const messageRehype: Options["rehypePlugins"] = [rehypeRaw, rehypeSanitize]

// #123, @login, and full commit IDs, not inside a word, a link, or code.
const reference =
  /(?<![\w/@#.-])(?:#(\d+)|@([A-Za-z\d](?:[A-Za-z\d]|-(?=[A-Za-z\d])){0,38})|\b([\da-f]{64}|[\da-f]{40}))(?![\w-])/g

/** A remark plugin that links references in text. */
function references(repo: string, github: string | null) {
  return () => (tree: Mdast) => {
    const walk = (node: Parent) => {
      if (node.type === "link" || node.type === "linkReference") return
      const out: typeof node.children = []
      for (const child of node.children) {
        if (child.type === "text") out.push(...split(child.value))
        else {
          if ("children" in child) walk(child as Parent)
          out.push(child)
        }
      }
      node.children = out
    }
    const split = (text: string): PhrasingContent[] => {
      const parts: PhrasingContent[] = []
      let at = 0
      for (const m of text.matchAll(reference)) {
        const [all, pull, login, oid] = m
        const url = pull
          ? `/${href(repo, "pull", pull)}`
          : login
            ? github && `https://github.com/${login}`
            : `/${href(repo, "commit", oid)}`
        if (!url) continue
        if (m.index > at)
          parts.push({ type: "text", value: text.slice(at, m.index) })
        const label = oid ? oid.slice(0, 7) : all
        parts.push({
          type: "link",
          url,
          children: [
            oid
              ? { type: "inlineCode", value: label }
              : { type: "text", value: label },
          ],
        })
        at = m.index + all.length
      }
      if (at < text.length) parts.push({ type: "text", value: text.slice(at) })
      return parts
    }
    walk(tree)
  }
}
