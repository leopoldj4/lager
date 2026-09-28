# Deployment on Vultr

Two or more servers behind a Vultr load balancer, all in one region, with
repository data in Vultr Object Storage. OpenTofu creates the
infrastructure; GitHub Actions builds the image; `rollout.sh` deploys it.

```text
git clients ──HTTPS──> load balancer (Let's Encrypt, /_rustgit/health)
                         │ VPC, port 8080
                         ├─> rustgit-1 (Docker, cache on local NVMe)
                         └─> rustgit-2
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
| `rollout.sh` | Installs the configuration and an image tag on each server, one at a time. |
| `terraform.tfvars` | Your values (not in Git). See `terraform.tfvars.example`. |
| `.secrets/` | Local only: plans, SSH known hosts, the first user's password. |

## Requirements

- `~/.vultr-cli.yaml` with an API key (`api-key: ...`), restricted to your
  IP address in the Vultr console.
- An Object Storage subscription labeled `rustgit` in the region. It
  passed `rustgit check-storage` (docs/TESTING.md); test again after any
  change of cluster or tier.
- `~/.ssh/rustgit_ed25519`: the deploy key for root on the servers.
- The dev shell (`nix develop`) provides `tofu`, `vultr-cli`, and `aws`.

## First deployment

1. Create the infrastructure without HTTPS:

   ```bash
   ./tf apply
   ```

2. Delegate the domain to Vultr DNS. At the DNS provider of the parent
   zone (Cloudflare for `leoj.de`), add the records from
   `./tf output delegation`: two `NS` records for `rustgit`. Wait until
   `dig +short rustgit.leoj.de` returns the load balancer address
   (`./tf output load_balancer_ip`).

3. Turn on HTTPS. The load balancer gets a Let's Encrypt certificate, and
   HTTP redirects to HTTPS:

   ```bash
   ./tf apply -var tls=true
   ```

   Then set `tls = true` in `terraform.tfvars`.

4. Configure GitHub Actions (`.github/workflows/image.yml`):

   ```bash
   gh variable set VULTR_REGISTRY --body "$(./tf output -raw registry)"
   ./tf output -raw registry_username | gh secret set VULTR_REGISTRY_USERNAME
   ./tf output -raw registry_password | gh secret set VULTR_REGISTRY_PASSWORD
   ```

   Push to `master`, or start the workflow by hand. Its summary names the
   image tag.

5. Deploy that tag:

   ```bash
   ./rollout.sh <tag>
   ```

6. Create repositories from your computer, with the server configuration:

   ```bash
   ./tf output -raw rustgit_config > .secrets/rustgit.toml
   nix run ../.. -- -c .secrets/rustgit.toml repo create myproject
   git clone https://leo@rustgit.leoj.de/myproject.git
   ```

## Changes

- **New release:** push to `master`, then `./rollout.sh <tag>`.
- **Users and settings:** edit `terraform.tfvars` or the template, then
  `./tf apply` (it changes only outputs) and `./rollout.sh <current tag>`.
  Password hashes come from `rustgit hash-password`.
- **More servers:** raise `server_count`, `./tf apply`, then
  `./rollout.sh <current tag>`.
- **Another admin address:** add it to `admin_cidrs` and `./tf apply`.

## Stopping and timeouts

A restart sends SIGTERM. The server fails its health check for 10 seconds
(`drain_delay_secs`; the load balancer needs 2 failed checks at 3-second
intervals), then finishes running requests for up to 180 seconds. Docker
waits 200 seconds and systemd 230 seconds before they kill it.

## Not included

No backups and no metrics collection. The servers keep no data: the
bucket is the only copy of the repositories.
