variable "name_prefix" {
  description = "Env-namespaced prefix for this shard's resource names (VPC/SG/IAM/ASG/launch-template), e.g. pollis-relay or pollis-relay-test. Region is appended."
  type        = string
}

variable "region" {
  description = "AWS region this pool shard runs in (must already be jurisdiction-approved by the root). Must match the region of the aws provider passed in."
  type        = string
}

variable "param_region" {
  description = <<-EOT
    Region holding the SSM parameters (the control plane's primary_region). SSM
    Parameter Store is regional and the pool's QUIC identity is minted once, in
    one region — so a node in ANY region fetches it from here, not from its own
    region. Node IAM and the KMS ViaService condition are scoped to this region
    for the same reason.
  EOT
  type        = string
}

variable "node_max" {
  description = "ASG max size. Set to the POOL-WIDE max: a random draw may legitimately place every node in this one region. Min size is 0 — the reconciler owns desired capacity."
  type        = number
}

variable "on_demand_base" {
  description = "on_demand_base_capacity: the first N nodes in this region are on-demand, the rest Spot. Bounds the blast radius of a regional Spot capacity event."
  type        = number
}

variable "instance_type" {
  description = "Graviton instance type."
  type        = string
}

variable "spot_max_price" {
  description = "Spot max price/hr (USD). Empty = cap at the on-demand price."
  type        = string
}

variable "relay_image" {
  description = "FALLBACK bootstrap image, used by user-data only if the intended-image SSM param is empty. Normally empty — the intended image is read from `image_param` at boot. Never `:latest`."
  type        = string
}

variable "image_param" {
  description = "SSM param name holding the intended build ({image, sha}). user-data reads .image at boot and launches THAT exact reference (no :latest in the launch path); the node IAM grants read on it. Written by CI on every roll, seeded by Terraform."
  type        = string
}

variable "relay_port" {
  type = number
}

variable "health_port" {
  type = number
}

variable "relay_allowlist" {
  type = string
}

variable "identity_key_param" {
  description = "SSM param name holding base64(raw) of the pool QUIC identity key."
  type        = string
}

variable "identity_cert_param" {
  description = "SSM param name holding base64(DER) of the pool QUIC leaf cert."
  type        = string
}

variable "health_source_cidr" {
  description = "CIDR allowed to reach the health TCP port. Default 0.0.0.0/0 because the reconciler Lambda has no fixed egress IP (VPC+NAT to pin it would blow the §0 budget). /health + /version expose only liveness + SHA. Lock to your egress CIDR if you have one."
  type        = string
  default     = "0.0.0.0/0"
}

variable "az_count" {
  description = "How many AZs to spread the public subnets (and thus nodes) across. Must not exceed what the account actually has in this region, and must be a STATIC number — the AZ name list is a data source, so it can't size a for_each. us-west-1 exposes only two AZs to most accounts and is passed 2 at the call site."
  type        = number
  default     = 3
}

variable "directory_key_b64" {
  description = "Pinned Ed25519 directory-signing PUBLIC key (base64, raw 32 bytes). Required for this node to act as a middle hop: without it the relay cannot evaluate revocation and so refuses to extend circuits (#813). Empty = single-hop only, which is honest rather than fail-open."
  type        = string
  default     = ""
}

variable "revocation_url" {
  description = "URL of the signed relay-revocation list (#813). Paired with directory_key_b64; both must be set for this node to extend circuits."
  type        = string
  default     = ""
}
