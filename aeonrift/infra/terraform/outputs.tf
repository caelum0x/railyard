# infra/terraform/outputs.tf — SCAFFOLD STUB outputs consumed by k8s + patcher tooling.

output "cluster_endpoints" {
  description = "Per-region Kubernetes API endpoints."
  value       = { for r, c in cloud_kubernetes_cluster.aeonrift : r => c.endpoint }
}

output "replays_bucket" {
  description = "Object bucket holding match replay input logs."
  value       = cloud_object_bucket.replays.name
}

output "patch_chunks_bucket" {
  description = "Object bucket holding content-addressed patch chunks."
  value       = cloud_object_bucket.patch_chunks.name
}

output "cdn_domain" {
  description = "Public CDN domain for patch + web asset delivery."
  value       = cloud_cdn_distribution.edge.domain
}
