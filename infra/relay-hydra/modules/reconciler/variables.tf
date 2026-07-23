variable "primary_region" {
  type = string
}

variable "managed_regions" {
  description = "region -> ASG name the reconciler drives."
  type        = map(string)
}

variable "reconcile_schedule" {
  type = string
}

variable "desired_state_param" {
  type = string
}

variable "desired_state_param_arn" {
  type = string
}

variable "signing_key_param" {
  type = string
}

variable "identity_cert_param" {
  type = string
}

variable "secret_param_arns" {
  description = "ARNs of the SecureString params the reconciler reads (signing key + identity key + identity cert)."
  type        = list(string)
}

variable "directory_bucket" {
  type = string
}

variable "directory_bucket_arn" {
  type = string
}

variable "directory_object_key" {
  type = string
}

variable "directory_ttl_seconds" {
  type = number
}

variable "relay_port" {
  type = number
}

variable "health_port" {
  type = number
}

variable "node_floor" {
  type = number
}

variable "node_max" {
  type = number
}
