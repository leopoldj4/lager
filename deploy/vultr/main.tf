locals {
  tls = var.tls_dir != null
  # The zone that holds the Git host name, at Cloudflare.
  parent_zone = join(".", slice(split(".", var.domain), 1, length(split(".", var.domain))))
}

data "vultr_object_storage" "main" {
  filter {
    name   = "label"
    values = [var.object_storage_label]
  }
}

resource "vultr_object_storage_bucket" "data" {
  object_storage_id = data.vultr_object_storage.main.id
  name              = var.bucket
}

resource "vultr_container_registry" "main" {
  name   = var.registry_name
  region = var.region
  plan   = "start_up"
  public = false
}

resource "vultr_vpc" "main" {
  region         = var.region
  description    = "rustgit"
  v4_subnet      = "10.42.0.0"
  v4_subnet_mask = 24
}

resource "vultr_ssh_key" "deploy" {
  name    = "rustgit-deploy"
  ssh_key = trimspace(file(pathexpand(var.ssh_public_key_file)))
}

# Public interfaces accept only SSH from the admin addresses (possibly
# every address; the servers accept only keys). The load
# balancer reaches ports 8080 (rustgit) and 3000 (management interface)
# through the VPC.
resource "vultr_firewall_group" "servers" {
  description = "rustgit servers"
}

resource "vultr_firewall_rule" "ssh" {
  for_each          = toset(var.admin_cidrs)
  firewall_group_id = vultr_firewall_group.servers.id
  protocol          = "tcp"
  ip_type           = "v4"
  subnet            = split("/", each.value)[0]
  subnet_size       = tonumber(split("/", each.value)[1])
  port              = "22"
  notes             = "admin ssh"
}

resource "vultr_firewall_rule" "git_from_vpc" {
  firewall_group_id = vultr_firewall_group.servers.id
  protocol          = "tcp"
  ip_type           = "v4"
  subnet            = vultr_vpc.main.v4_subnet
  subnet_size       = vultr_vpc.main.v4_subnet_mask
  port              = "8080"
  notes             = "load balancer to rustgit"
}

resource "vultr_firewall_rule" "web_from_vpc" {
  firewall_group_id = vultr_firewall_group.servers.id
  protocol          = "tcp"
  ip_type           = "v4"
  subnet            = vultr_vpc.main.v4_subnet
  subnet_size       = vultr_vpc.main.v4_subnet_mask
  port              = "3000"
  notes             = "load balancer to the management interface"
}

data "vultr_os" "debian" {
  filter {
    name   = "name"
    values = ["Debian 13 x64 (trixie)"]
  }
}

# cloud-init installs Docker and the rustgit unit only. Configuration,
# registry login, and the image arrive with ./rollout.sh, so no secret
# is in the instance metadata.
resource "vultr_instance" "server" {
  count             = var.server_count
  region            = var.region
  plan              = var.server_plan
  os_id             = data.vultr_os.debian.id
  label             = "rustgit-${count.index + 1}"
  hostname          = "rustgit-${count.index + 1}"
  vpc_ids           = [vultr_vpc.main.id]
  firewall_group_id = vultr_firewall_group.servers.id
  ssh_key_ids       = [vultr_ssh_key.deploy.id]
  backups           = "disabled"
  enable_ipv6       = false
  user_data = templatefile("${path.module}/cloud-init.yaml.tftpl", {
    container_memory = var.container_memory
  })
}

resource "vultr_load_balancer" "main" {
  region              = var.region
  label               = "rustgit"
  vpc                 = vultr_vpc.main.id
  attached_instances  = sort(vultr_instance.server[*].id)
  balancing_algorithm = "roundrobin"
  ssl_redirect        = local.tls

  # Vultr's automatic certificates need the parent zone in Vultr DNS, and
  # Vultr DNS refuses subdomain zones. ./cert.sh gets the certificate
  # through the parent zone's DNS instead.
  dynamic "ssl" {
    for_each = local.tls ? [1] : []
    content {
      certificate = file("${var.tls_dir}/cert.pem")
      private_key = file("${var.tls_dir}/key.pem")
      chain       = file("${var.tls_dir}/chain.pem")
    }
  }

  forwarding_rules {
    frontend_protocol = "http"
    frontend_port     = 80
    backend_protocol  = "http"
    backend_port      = 8080
  }

  dynamic "forwarding_rules" {
    for_each = local.tls ? [1] : []
    content {
      frontend_protocol = "https"
      frontend_port     = 443
      backend_protocol  = "http"
      backend_port      = 8080
    }
  }

  # The management interface (web/), only over HTTPS: its sign-in form
  # carries passwords. The health check below covers rustgit only.
  dynamic "forwarding_rules" {
    for_each = local.tls ? [1] : []
    content {
      frontend_protocol = "https"
      frontend_port     = var.web_port
      backend_protocol  = "http"
      backend_port      = 3000
    }
  }

  # A server that stops fails this check within about 6 seconds, less
  # than drain_delay_secs in the rustgit configuration.
  health_check {
    protocol            = "http"
    port                = 8080
    path                = "/_rustgit/health"
    check_interval      = 3
    response_timeout    = 2
    unhealthy_threshold = 2
    healthy_threshold   = 2
  }
}

data "cloudflare_zone" "parent" {
  filter = {
    name = local.parent_zone
  }
}

resource "cloudflare_dns_record" "git" {
  zone_id = data.cloudflare_zone.parent.id
  name    = var.domain
  type    = "A"
  content = vultr_load_balancer.main.ipv4
  ttl     = 300
  # Not proxied: Cloudflare's proxy limits requests to 100 MB, which
  # large pushes exceed, and would terminate TLS itself.
  proxied = false
  comment = "rustgit load balancer (deploy/vultr)"
}
