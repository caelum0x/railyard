# infra/terraform/main.tf — SCAFFOLD STUB. Illustrative only; not applied.
# Providers are declared abstractly; real credentials come from a secret manager, never git.

terraform {
  required_version = ">= 1.6.0"

  # Remote, locked state (placeholder backend — configure per environment).
  backend "s3" {
    bucket = "PLACEHOLDER-aeonrift-tfstate"
    key    = "infra/terraform.tfstate"
    region = "us-east-1"
  }

  required_providers {
    cloud = {
      source  = "hashicorp/PLACEHOLDER"
      version = "~> 5.0"
    }
  }
}

# Kubernetes cluster that hosts services/* (one per region).
resource "cloud_kubernetes_cluster" "aeonrift" {
  for_each = toset(var.regions)
  name     = "aeonrift-${each.key}"
  region   = each.key
  version  = var.k8s_version
  node_pool {
    size  = var.node_size
    count = var.node_count
  }
}

# Object storage / R2 buckets: replays, patch chunks, exports.
resource "cloud_object_bucket" "replays" {
  name = "aeonrift-replays-${var.environment}"
}

resource "cloud_object_bucket" "patch_chunks" {
  name = "aeonrift-patch-chunks-${var.environment}"
}

# CDN distribution fronting the patch chunks + web static assets.
resource "cloud_cdn_distribution" "edge" {
  origin_bucket = cloud_object_bucket.patch_chunks.name
  domain        = var.cdn_domain
}
