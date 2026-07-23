variable "region" {
  description = "AWS region this pool shard runs in (must already be jurisdiction-approved by the root)."
  type        = string
}

variable "node_floor" {
  description = "ASG min size AND on_demand_base_capacity — the guaranteed-on-demand floor Spot can never drop below."
  type        = number
}

variable "node_max" {
  description = "ASG max size."
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
  type = string
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
  description = "How many AZs to spread the public subnets (and thus nodes) across."
  type        = number
  default     = 3
}
