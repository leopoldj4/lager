# Deployment on Vultr

Two or more servers behind a Vultr load balancer, all in one region, with
repository data in Vultr Object Storage. OpenTofu creates the
infrastructure; GitHub Actions builds the images; `rollout.sh` deploys them.
Each server also runs the management interface (`web/`) in a second
container.

```text
git clients ──HTTPS :443──> load balancer (certificate from cert.sh, /_rustgit/health)
browsers ────HTTPS :8443─>   │ VPC, ports 8080 and 3000
                             ├─> rustgit-1: rustgit :8080 (cache on local NVMe)
                             │              rustgit-web :3000 ──> rustgit :8080
                             └─> rustgit-2: the same
                                   │
                                   └─> Vultr Object Storage (bucket, prefix "prod")
```

Request forwarding between servers is off: every request checks the
storage head, so any server can answer any request. Enable `[routing]`
only if cache rebuilds show up as a cost.

## Files

| File | Purpose |
|---|---|
| `versions.tf`, `main.tf`, `variables.tf`, `outputs.tf` | Infrastructure. State is in the versioned bucket `rustgit-tfstate-9255cf08`. |
| `cloud-init.yaml.tftpl` | Host setup: Docker and the `rustgit` systemd unit. No secrets. |
| `rustgit.toml.tftpl` | Server configuration, rendered into the sensitive output `rustgit_config`. |
| `tf` | Runs `tofu` with the Vultr API key and the state bucket's keys. |
| `rollout.sh` | Installs the configuration and an image tag on each server, one at a time; with `--web`, the management interface. |
| `cert.sh` | Gets or renews the Let's Encrypt certificate (DNS-01 at Cloudflare) and installs it on the load balancer. |
| `terraform.tfvars` | Your values (not in Git). See `terraform.tfvars.example`. |
| `.secrets/` | Local only: plans, SSH known hosts, the first user's password, the interface's session secret. |

## Requirements

- `~/.vultr-cli.yaml` with an API key (`api-key: ...`). Its access control
  in the Vultr console (Account → API) allows every address, `0.0.0.0/0`
  and `::/0`, so deployments work from any network. The key then has full
  account access from anywhere: keep the file readable only by you
  (`chmod 600`), and regenerate the key if it may have leaked.
- An Object Storage subscription labeled `rustgit` in the region. It
  passed `rustgit check-storage` (docs/TESTING.md); test again after any
  change of cluster or tier.
- `~/.ssh/rustgit_ed25519`: the deploy key for root on the servers.
- `.secrets/cloudflare-token`: a Cloudflare API token with Zone Read and
  DNS Edit for the parent zone (`leoj.de`). OpenTofu creates the host's A
  record with it, and `cert.sh` answers the certificate challenge.
- The dev shell (`nix develop`) provides `tofu`, `vultr-cli`, and `aws`.

## First deployment

1. Create the infrastructure and the DNS record, without HTTPS:

   ```bash
   ./tf apply
   ```

   The A record at Cloudflare is not proxied: Cloudflare's proxy limits
   requests to 100 MB, which large pushes exceed.

2. Get the certificate and turn on HTTPS. This accepts the Let's Encrypt
   terms of service. HTTP then redirects to HTTPS.

   ```bash
   ./cert.sh
   ```

   Vultr's automatic certificates are not usable: they need the parent
   zone in Vultr DNS, and Vultr DNS refuses subdomain zones.

3. Configure GitHub Actions (`.github/workflows/image.yml`):

   ```bash
   gh variable set VULTR_REGISTRY --body "$(./tf output -raw registry)"
   ./tf output -raw registry_username | gh secret set VULTR_REGISTRY_USERNAME
   ./tf output -raw registry_password | gh secret set VULTR_REGISTRY_PASSWORD
   ```

   Push to `master`, or start the workflows by hand (`image.yml` for
   rustgit, `web-image.yml` for the interface). Their summaries name the
   image tags.

4. Deploy the tags:

   ```bash
   ./rollout.sh <tag>
   ./rollout.sh --web <web tag>
   ```

   The interface is then at `./tf output -raw web_url`
   (`https://<domain>:8443`). Sign in as the first user.

5. Create repositories from your computer, with the server configuration:

   ```bash
   ./tf output -raw rustgit_config > .secrets/rustgit.toml
   nix run ../.. -- -c .secrets/rustgit.toml repo create myproject
   git clone https://leo@rustgit.leoj.de/myproject.git
   ```

## Changes

- **New release:** push to `master`, then `./rollout.sh <tag>` and, if
  `web/` changed, `./rollout.sh --web <web tag>`.
- **Users:** create and change them in the interface; they are kept in
  storage and need no rollout. Users in `terraform.tfvars` take precedence
  and are read-only there.
- **Configured users and settings:** edit `terraform.tfvars` or the template, then
  `./tf apply` (it changes only outputs) and `./rollout.sh <current tag>`.
  Password hashes come from `rustgit hash-password`.
- **More servers:** raise `server_count`, `./tf apply`, then
  `./rollout.sh <current tag>`.
- **Another admin address:** add it to `admin_cidrs` and `./tf apply`.
  This is SSH access to the servers; the API key works from anywhere.
- **Certificate:** run `./cert.sh` every month. It renews when fewer than
  30 days are left.

## Stopping and timeouts

A restart sends SIGTERM. The server fails its health check for 10 seconds
(`drain_delay_secs`; the load balancer needs 2 failed checks at 3-second
intervals), then finishes running requests for up to 180 seconds. Docker
waits 200 seconds and systemd 230 seconds before they kill it.

## Management interface

`rollout.sh --web` installs the `rustgit-web` unit, `/etc/rustgit-web/web.env`
(with the session secret from `.secrets/web-session-secret`, created on
first use and the same on every server), and the image. The container calls
rustgit on its own server. It is public on port 8443, only over HTTPS;
sign-in needs a rustgit user with `"*" = "admin"`, and failed sign-ins are
not rate-limited, so give such users long random passwords. The load
balancer's health check covers rustgit only.

## Not included

No backups and no metrics collection. The servers keep no data: the
bucket is the only copy of the repositories.
