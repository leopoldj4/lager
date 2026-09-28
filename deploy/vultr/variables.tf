variable "region" {
  description = "Vultr region of servers, load balancer, and registry. The Object Storage must be in the same region."
  type        = string
  default     = "ewr"
}

variable "domain" {
  description = "Git host name. Point it at the load balancer with an A record."
  type        = string
}

variable "tls_dir" {
  description = "Directory with cert.pem, key.pem, and chain.pem for the domain (./cert.sh). Without it, the load balancer serves only HTTP."
  type        = string
  default     = null
}

variable "server_count" {
  type    = number
  default = 2
}

variable "server_plan" {
  description = "High-frequency plans have local NVMe for the cache."
  type        = string
  default     = "vhf-2c-4gb"
}

variable "container_memory" {
  description = "Memory limit of the rustgit container. Leave room for the system."
  type        = string
  default     = "3500m"
}

variable "object_storage_label" {
  description = "Label of the existing Object Storage subscription."
  type        = string
  default     = "rustgit"
}

variable "bucket" {
  description = "Bucket for repository data."
  type        = string
}

variable "registry_name" {
  description = "Container registry name: lowercase letters and digits, unique at Vultr."
  type        = string
}

variable "admin_cidrs" {
  description = "Addresses allowed to connect with SSH, for example [\"203.0.113.7/32\"]."
  type        = list(string)
}

variable "ssh_public_key_file" {
  type    = string
  default = "~/.ssh/rustgit_ed25519.pub"
}

variable "users" {
  description = "Git users. Hashes come from `rustgit hash-password`."
  type = list(object({
    name          = string
    password_hash = string
    access        = map(string)
  }))
}
