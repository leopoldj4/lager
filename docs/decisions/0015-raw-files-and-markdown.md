# 15. Raw files and Markdown

Status: accepted; implemented in `crates/server/src/web/raw.rs` and
`web/src/components/repo/markdown.tsx`. Date: October 13, 2026.

## Context

READMEs and other Markdown files were shown as plain text. Rendered
Markdown needs its relative images, which the browser loads by URL, but
files reached the browser only as `blob` queries over the live connection
(decision 14), at most 1 MiB and as text. Files come from repositories,
so what they contain must not run on the site that holds the session.

## Decision

**Raw files.** `GET /<name>/-/raw/<rev>/<path>` answers a file's bytes.
`<rev>/<path>` is split at the longest branch or tag name, as the pages
split theirs. Access, the customer's request limit, and the Git process
slot apply as for queries; unknown repositories and missing permission
are both 404. Files above 20 MiB are refused (403). The blob's object ID
is the `ETag`; a matching `If-None-Match` answers 304 without reading the
file. `Cache-Control: private, no-cache`.

**Content types.** Images (PNG, JPEG, GIF, WebP, AVIF, SVG, ICO) and
videos (MP4, WebM) keep their type; other UTF-8 text without NUL bytes is
`text/plain`, everything else `application/octet-stream`. HTML is never
served as HTML. Every answer has `X-Content-Type-Options: nosniff` and
`Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline';
sandbox`, so an SVG opened directly runs no scripts.

**Session cookie on the whole site.** The cookie's path becomes `/`
instead of `/-/api`, so image requests carry it. Git requests now carry it
too; Git authentication ignores it. Signing in and out also removes a
cookie of the old path.

**Markdown.** `react-markdown` with `remark-gfm` renders READMEs under a
directory and `.md` files, which can switch to their source. HTML in
Markdown is left out rather than sanitized. Relative links open the
repository's pages (a file page that names a directory moves to the
directory page), and relative images load from the raw route at the same
revision.

## Consequences

- Sessions signed in before this change keep working for the live
  connection, but images in private repositories load only after signing
  in again.
- READMEs that lay out content with HTML (`<p align="center">`, `<img>`,
  `<details>`) lose that part. `rehype-raw` with `rehype-sanitize` can add
  it later.
- Raw files share the origin of the interface. The CSP sandbox and
  `nosniff` make that safe for the types above; a separate domain for raw
  files would be needed before serving HTML or PDF as such.
- Images from other sites are loaded directly by the browser and see the
  viewer's address.
- Headings have no anchors yet, and code blocks are not highlighted.
