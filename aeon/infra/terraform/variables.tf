# infra/terraform/variables.tf — SCAFFOLD STUB inputs.

variable "environment" {
  description = "Deployment environment (dev|staging|prod)."
  type        = string
  default     = "dev"
}

variable "regions" {
  description = "Regions to deploy clusters + fleets into."
  type        = list(string)
  default     = ["us-east-1", "eu-west-1", "ap-southeast-1"]
}

variable "k8s_version" {
  description = "Kubernetes version for the managed clusters."
  type        = string
  default     = "1.30"
}

variable "node_size" {
  description = "Instance size for the default node pool."
  type        = string
  default     = "c-standard-8"
}

variable "node_count" {
  description = "Default node pool size per cluster."
  type        = number
  default     = 3
}

variable "cdn_domain" {
  description = "CDN domain fronting patch chunks + web assets."
  type        = string
  default     = "cdn.ereboslab.example"
}
