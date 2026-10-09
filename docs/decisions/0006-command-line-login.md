# 6. Command-line login and repository changes through the API

Status: accepted. Date: September 29, 2026.

## Context

`rustgit repo` commands read the server configuration and change storage
directly, so managing repositories from a laptop needed the storage keys
there. Everyone who manages repositories already has a rustgit user.

## Decision

**API.** The admin API (decision 4) gains repository changes, with the same
rule: admin access to every repository. They need the storage catalog;
otherwise the answer is 409.

| Request | Answer |
|---|---|
| `POST repos` | `{name, default_branch?}`; 201 with the repository |
| `PATCH repos/<name>` | `{default_branch}`; the history, then the catalog record |
| `DELETE repos/<name>` | 204; tombstone head, then the record |

`<name>` may contain `/`. The changes are those of the storage commands and
share their code (`Catalog::create`, `set_default_branch`, `delete`).

**Client.** `rustgit login <url>` asks for the user name and password (the
password without echo; both from standard input when it is not a
terminal), checks them with `GET me`, and only then writes
`$XDG_CONFIG_HOME/rustgit/login.toml` (default `~/.config`) with mode 0600.
`rustgit logout` removes it. Without `-c` or `RUSTGIT_CONFIG`, `repo`
commands use the saved login if there is one, else `rustgit.toml` as
before.

## Consequences

- The password is kept in plain text in a file only the user can read, as
  Git's `credential-store` does. The server has no tokens to use instead.
- Other storage commands (`snapshot`, `cleanup`, `backup`) still need the
  configuration.
