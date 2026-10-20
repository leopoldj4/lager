# 20. Repository overview and the application frame

Status: accepted; implemented in `web/src/components/repo/overview.tsx`,
`web/src/components/repo/code.tsx`, `web/src/components/app-sidebar.tsx`,
and `web/src/components/omnibar.tsx`.
Date: October 15, 2026.

## Context

`/<name>` showed the files of the default branch (decision 14). Since
decision 19, work on a repository happens in its pull requests, which were
one tab away. Every page had a header with only the product name and the
user menu, so moving between repositories went through the list of all
repositories.

## Decision

**Overview.** `/<name>` shows the repository's overview: the signed-in
user's open pull requests, the other open pull requests, the latest commits
of the default branch, and the README of its root directory. A side column
lists the branches pushed last, each with its open pull request or a link
that opens one, and the latest tag. It reads the existing queries (`refs`,
`pulls`, `log`, `tree`, `blob`); the server is unchanged.

**Code moves to a tab.** The files are at `/<name>/-/tree`, which shows the
default branch without a `<rev>`. The tabs are Overview, Pull requests,
and Code. History and branches have no tab: the overview and the
code link to them, and they mark the Code tab. Addresses are unchanged.

**Code view.** The Code tab uses the whole width: a file tree beside the
directory or file the address names, in panels the user can resize and
hide (`⌘\` or `Ctrl+\`); the browser keeps their sizes. The tree loads a
directory when it opens, with the `tree` query, and opens the directories
above the current path. Both panels scroll on their own. On narrow screens
the tree opens in a sheet.

**Sidebar.** The header is replaced by a sidebar on every page (shadcn
`Sidebar`): the product name, search, the list of repositories, the user's
own (`<user>/…`, decision 18) apart from the others, with a count of open
pull requests each, the connection status, and the user menu. On narrow
screens it is a sheet behind a menu button.

**Search.** `⌘K` or `Ctrl+K`, or the field in the sidebar, opens a command
dialog (shadcn `Command`, cmdk). On a repository page it searches that
repository first: open pull requests (by number, title, branch, or
author), its pages, and its branches; Backspace in the empty field removes
the scope. Repositories and account actions are always listed. Filtering
runs in the browser over answers the page already has or reads once.

## Consequences

- Links to `/<name>` that meant the files now open the overview.
- The sidebar reads the open pull request count of each repository it
  lists, one `pulls` query each, at most eight per group. The counts
  refresh when a page of that repository is open, otherwise when the cache
  is stale.
- Search finds no files, commits, or closed pull requests: that needs
  queries the server does not have yet.
