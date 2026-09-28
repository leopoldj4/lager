#!/usr/bin/env bash
# Gets or renews the Let's Encrypt certificate for the Git host with a
# DNS-01 challenge at Cloudflare, then installs it on the load balancer.
# Run it every month; lego renews when fewer than 30 days are left.
# Using it accepts the Let's Encrypt terms of service.
set -euo pipefail
cd "$(dirname "$0")"
domain=$(sed -n 's/^domain *= *"\(.*\)"/\1/p' terraform.tfvars)
token=$(<.secrets/cloudflare-token)
export CF_DNS_API_TOKEN=$token CF_ZONE_API_TOKEN=$token

lego run --accept-tos --dns cloudflare --domains "$domain" \
  --key-type rsa2048 --no-bundle --renew-days 30 --no-random-sleep \
  --path .secrets/lego

# The files that tls_dir names.
src=.secrets/lego/certificates
mkdir -p .secrets/tls
cp "$src/$domain.crt" .secrets/tls/cert.pem
cp "$src/$domain.issuer.crt" .secrets/tls/chain.pem
cp "$src/$domain.key" .secrets/tls/key.pem
chmod 600 .secrets/tls/*.pem

# Later applies keep the certificate.
grep -q '^tls_dir' terraform.tfvars || echo 'tls_dir = ".secrets/tls"' >> terraform.tfvars
./tf apply
