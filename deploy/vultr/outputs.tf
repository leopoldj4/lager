output "load_balancer_ip" {
  value = vultr_load_balancer.main.ipv4
}

output "server_ips" {
  value = vultr_instance.server[*].main_ip
}

output "registry" {
  description = "Image repository for docker push and pull."
  value       = "${vultr_container_registry.main.urn}/rustgit"
}

output "registry_username" {
  value     = vultr_container_registry.main.root_user["username"]
  sensitive = true
}

output "registry_password" {
  value     = vultr_container_registry.main.root_user["password"]
  sensitive = true
}

output "rustgit_config" {
  description = "The server configuration. Contains storage keys."
  sensitive   = true
  value = templatefile("${path.module}/rustgit.toml.tftpl", {
    s3_endpoint = "https://${data.vultr_object_storage.main.s3_hostname}"
    bucket      = vultr_object_storage_bucket.data.name
    access_key  = data.vultr_object_storage.main.s3_access_key
    secret_key  = data.vultr_object_storage.main.s3_secret_key
    users       = var.users
  })
}
