# ── Environment ─────────────────────────────────────────────────────────────

variable "env" {
  description = <<-EOT
    Deployment environment. "prod" (the default) reproduces the ORIGINAL resource
    names exactly, so re-applying prod is a no-op. Any other value (e.g. "test")
    namespaces every named resource — S3 bucket, SSM params, Lambda, ASG, IAM
    roles, SG, alarms, budget — so a second isolated pool can stand up in the SAME
    account+region without colliding. Point a non-prod env at your dev/test hosts
    (relay_allowlist) + a throwaway signing key to exercise the real AWS infra end
    to end, then `terraform destroy` it.
  EOT
  type        = string
  default     = "prod"
}

# ── Pool sizing & placement ─────────────────────────────────────────────────

variable "primary_region" {
  description = "AWS region the whole stack (VPC/ASG/Lambda/S3) is created in. Must be an allowed region (see jurisdiction.tf)."
  type        = string
  default     = "us-west-2"
}

variable "region_node_counts" {
  description = <<-EOT
    Desired-state: per-region relay node count. This SEEDS the SSM desired-state
    parameter; after apply the reconciler owns runtime scaling and Terraform
    ignores drift on the seeded value (edit the SSM param to scale — see README).
    Each key must be an allowed region. Each value is clamped to [node_floor, node_max].
  EOT
  type        = map(number)
  default     = { "us-west-2" = 3 }
}

variable "node_floor" {
  description = "Minimum always-on nodes per region. Wired to the ASG min size AND on_demand_base_capacity so Spot reclamation can never take the pool below it."
  type        = number
  default     = 2
}

variable "node_max" {
  description = "Hard per-region ASG max. Sized to the §0 budget math (~$5-6/node all-in). Do not raise without re-checking the $20/mo cap."
  type        = number
  default     = 3
}

# ── Jurisdiction (state-based denylist, §4) ─────────────────────────────────

variable "state_denylist" {
  description = "US states denied for placement: any state with an age-verification or device/OS-level age-registration law. A region is excluded iff its AZs sit in a denied state."
  type        = set(string)
  default     = ["Virginia", "Ohio", "California"]
}

variable "region_state_map" {
  description = "AWS region -> US state its AZs sit in. This is the policy source of truth: a future region add/remove is a one-line edit here, re-checked against state_denylist."
  type        = map(string)
  default = {
    "us-east-1" = "Virginia"
    "us-east-2" = "Ohio"
    "us-west-1" = "California"
    "us-west-2" = "Oregon"
  }
}

# ── Relay image & runtime ───────────────────────────────────────────────────

variable "relay_image" {
  description = "Container image the nodes run. Must be pullable by the nodes (make the GHCR package public, or add a pull secret)."
  type        = string
  default     = "ghcr.io/actuallydan/pollis-relay:latest"
}

variable "relay_port" {
  description = "UDP port clients dial the QUIC relay on (POLLIS_RELAY_BIND)."
  type        = number
  default     = 9444
}

variable "health_port" {
  description = "TCP port for the relay's /health + /version endpoint (POLLIS_RELAY_HEALTH_BIND)."
  type        = number
  default     = 9445
}

variable "relay_allowlist" {
  description = <<-EOT
    The four first-party destinations the relay forwards to (POLLIS_RELAY_ALLOWLIST),
    as a comma-separated hostname list. Defaults are pulled from .env.production
    (TURSO_URL, VITE_SERVICE_URL, R2_S3_ENDPOINT + R2_PUBLIC_URL, LIVEKIT_URL) with
    schemes/paths stripped. Verify against the CURRENT .env.production before apply.
  EOT
  type        = string
  default     = "prod-actuallydan.aws-us-east-1.turso.io,api.pollis.com,4bd9ab176c5febd5e7ac1b64b23dede5.r2.cloudflarestorage.com,cdn.pollis.com,rtc.pollis.com"
}

variable "instance_type" {
  description = "Graviton instance type. t4g.nano is the §0 default; anything larger blows the budget."
  type        = string
  default     = "t4g.nano"
}

variable "spot_max_price" {
  description = "Spot max price per hour (USD) as a hard cost cap. Empty string = on-demand price cap (recommended: pay Spot market, never above on-demand)."
  type        = string
  default     = ""
}

# ── Directory hosting ───────────────────────────────────────────────────────

variable "directory_domain" {
  description = "Stable custom domain the client bakes in as POLLIS_OVERLAY_DIRECTORY_URL host. Requires a DNS CNAME to CloudFront + ACM DNS validation (see README). Set to \"\" to use the raw *.cloudfront.net domain instead."
  type        = string
  default     = "relays.pollis.com"
}

variable "directory_object_key" {
  description = "S3 object key / URL path the signed directory is published at."
  type        = string
  default     = "directory.json"
}

variable "directory_ttl_seconds" {
  description = "expires_at - issued_at for each signed directory. Short so a stale/rolled-back directory expires quickly."
  type        = number
  default     = 3600
}

# ── Reconciler ──────────────────────────────────────────────────────────────

variable "reconcile_schedule" {
  description = "EventBridge schedule expression for the reconciler."
  type        = string
  default     = "rate(2 minutes)"
}

# ── Guardrails ──────────────────────────────────────────────────────────────

variable "monthly_budget_usd" {
  description = "AWS Budgets threshold (the §0 hard target). Alerts at forecasted + actual breach."
  type        = number
  default     = 20
}

variable "budget_alert_emails" {
  description = "Emails to notify on the Budgets alert. Empty = no email subscribers (the budget still exists in the console)."
  type        = list(string)
  default     = []
}

variable "alarm_email_addresses" {
  description = "Emails subscribed to the SNS topic the CloudWatch alarms (reconcile failures, Lambda errors, healthy-node floor) notify. Each address must confirm the AWS subscription email. Empty = alarms fire to the topic but nobody is subscribed."
  type        = list(string)
  default     = []
}
