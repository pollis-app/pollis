variable "name_prefix" {
  description = "Env-namespaced resource-name prefix (e.g. pollis-relay-hydra or pollis-relay-hydra-test). Names the Lambda, IAM role, SNS topic, event rule, and alarms."
  type        = string
}

variable "metric_namespace" {
  description = "CloudWatch metric namespace the reconciler emits into and the alarms read from. Per-env so two pools never cross wires."
  type        = string
}

variable "primary_region" {
  type = string
}

variable "managed_regions" {
  description = "region -> ASG name the reconciler drives. This IS the set the random placement draws from — every allowed region appears here, whether or not it currently holds nodes."
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

variable "placement_param" {
  description = "SSM param holding the current random region draw ({drawn_at, placement}). The reconciler is the only writer."
  type        = string
}

variable "placement_param_arn" {
  type = string
}

variable "intended_image_param" {
  description = "SSM param holding the intended build ({image, sha}), written by CI on every image roll. The reconciler reads .sha to identify build-stale nodes."
  type        = string
}

variable "intended_image_param_arn" {
  type = string
}

variable "expected_relay_protocol" {
  description = "Expected relay ALPN/wire identity (e.g. pollis-relay/3). A healthy node reporting a different protocol at /version is excluded from the signed directory. Empty disables the protocol membership gate."
  type        = string
}

variable "max_cycle_per_run" {
  description = "Max build-stale nodes the reconciler cycles per reconcile. Bounds a roll's rate; the floor guard additionally prevents emptying the pool."
  type        = number
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
  description = "Pool-wide minimum node count. The reconciler clamps desired-state up to it, and the healthy-nodes alarm fires below it."
  type        = number
}

variable "node_max" {
  description = "Pool-wide maximum node count. The reconciler clamps desired-state down to it."
  type        = number
}

variable "rotation_interval_hours" {
  description = "How often the reconciler re-draws the random region placement."
  type        = number
}

variable "alarm_email_addresses" {
  description = "Emails subscribed to the SNS topic the CloudWatch alarms notify. Each address must confirm the subscription via the email AWS sends. Empty = the topic still exists and alarms still fire to it, but nobody is subscribed."
  type        = list(string)
  default     = []
}
