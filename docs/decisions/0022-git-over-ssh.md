# 22. Git over SSH

Status: accepted; implemented in `crates/server/src/ssh.rs` and
`crates/core/src/identity/`. Date: October 15, 2026.

## Context

Git clients spoke only smart HTTP. Many people keep SSH keys for Git and
expect `git@host:org/app.git` URLs; HTTP needs a token in a credential
helper. The design kept SSH as a later extension. Git's SSH transport runs
the same `git-upload-pack` and `git-receive-pack` as smart HTTP, so the
push procedure (decision 1), the access checks, and the limits apply
unchanged.

## Decision

**Server.** With `[ssh]`, each server also accepts SSH on `ssh.listen`,
in the same process, with `russh`. A connection signs in only with a
public key; the SSH user name is ignored, so `git@` works. A session
channel runs one command, `git-upload-pack '<name>'` or
`git-receive-pack '<name>'` (also `git upload-pack`), with or without a
leading `/` and `.git`. Everything else (shells, terminals, forwarding,
other commands, other quoting) is refused with a message and exit status 1.
The Git process runs without `--stateless-rpc`, connected to the channel,
with the same permission check, create-on-push, process permit, CPU and
memory limits, and `receive.maxInputSize` as smart HTTP. A push reserves
the default disk amount, since SSH gives no size in advance. A fetch keeps
one read view for the whole connection. `GIT_PROTOCOL` is taken from an
`env` request and loses `version=2`, as the HTTP header does.

**Host key.** `ssh.host_key` names an OpenSSH private key without a
passphrase. Every server of a deployment uses the same file: a load
balancer sends a client to any server, and a changed host key stops
clients. The key belongs with the other startup settings in the file
(decision 24), not in the database.

**Keys.** PostgreSQL holds public keys in `lager_ssh_keys` (schema
version 6): user, name, SHA-256 fingerprint as `ssh-keygen -l` shows it,
the key without its comment, creation and last use (updated at most once a
minute). A fingerprint is unique across users, since a key signs in as
one user. A user has at most 100 keys. A key has its user's full access;
limited access stays with tokens (decision 9). Accepted types: Ed25519,
ECDSA, and RSA; not DSA, security keys, or certificates.
`lager ssh-key add <user> <file|->`, `list`, and `delete` manage them;
deleting the user deletes its keys.
Signed-in users manage their own keys on the settings page
(`/-/settings/ssh-keys`) through `GET`, `POST`, and `DELETE`
`/-/api/ssh-keys`. Adding a key needs no password, as creating a token on
the web does not; changing requests must come from this site.

**Web interface.** `ssh.public_url` (such as `ssh://git@git.example.com`)
is answered by `GET /-/api/ssh`, and the clone menu shows the SSH URL. On
port 22 it uses the short `git@host:org/app.git` form.

**Stop.** On a clean stop the SSH listener closes with the HTTP one.
Connections without a running Git process are disconnected; running
commands finish within the shutdown timeout.

## Consequences

- The deployment publishes port 22 for lager; the host's own SSH server
  moves to another port first.
- Log entries record the user name, as for HTTP. The request log has one
  `ssh request` record per command with its exit status.
- A key works for Git only; there is no web sign-in with SSH keys.
- Clients that need SSH certificates or security keys use HTTP for now.
