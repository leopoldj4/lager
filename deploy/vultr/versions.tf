terraform {
  required_version = ">= 1.10"

  required_providers {
    vultr = {
      source  = "vultr/vultr"
      version = "~> 2.0"
    }
  }

  # State in Vultr Object Storage. The bucket keeps old versions, and the
  # lock file uses conditional writes, which this provider supports
  # (docs/TESTING.md). Credentials come from ./tf.
  backend "s3" {
    bucket = "rustgit-tfstate-9255cf08"
    key    = "vultr/terraform.tfstate"
    region = "us-east-1"
    endpoints = {
      s3 = "https://ewr1.vultrobjects.com"
    }
    use_path_style              = true
    use_lockfile                = true
    skip_credentials_validation = true
    skip_region_validation      = true
    skip_requesting_account_id  = true
    skip_metadata_api_check     = true
    skip_s3_checksum            = true
  }
}

# The API key comes from VULTR_API_KEY (./tf reads ~/.vultr-cli.yaml).
provider "vultr" {}
