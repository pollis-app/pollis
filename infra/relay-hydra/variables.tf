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
  description = <<-EOT
    AWS region the CONTROL PLANE lives in — the reconciler Lambda, the directory
    S3 bucket, and every SSM parameter (secrets + desired-state + placement).
    Relay NODES are no longer confined to it: they are placed across the allowed
    regions at random (see region_placement below). Must itself be an allowed
    region, since the control plane sees relay IPs.
  EOT
  type        = string
  default     = "us-west-2"
}

variable "pool_node_count" {
  description = <<-EOT
    Desired-state: the POOL-WIDE relay node count, across all regions. This SEEDS
    the SSM desired-state parameter; after apply the reconciler owns runtime
    scaling and Terraform ignores drift on the seeded value (edit the SSM param to
    scale — see README). Clamped to [node_floor, node_max].

    Regions are NOT specified here. The reconciler draws each node's region at
    random from the allowed set on every rotation; two nodes may land in the same
    region.
  EOT
  type        = number
  default     = 3
}

variable "node_floor" {
  description = "Minimum POOL-WIDE nodes. The reconciler clamps desired-state up to this, so the pool can never be scaled to zero by an edit."
  type        = number
  default     = 2
}

variable "node_max" {
  description = "Hard POOL-WIDE cap, and each region's ASG max (one region may legitimately hold the whole pool after a random draw). Sized to the §0 budget math (~$5-6/node all-in). Do not raise without re-checking the $20/mo cap."
  type        = number
  default     = 3
}

variable "on_demand_base_per_region" {
  description = <<-EOT
    Per-region on_demand_base_capacity: the first N nodes in any occupied region
    are on-demand, the rest Spot.

    DEFAULT IS 0 (all Spot) because the §0 <$20/mo target is hard and this knob is
    per-REGION, so its cost scales with how wide the draw spreads: at 1, a 3-node
    pool that lands in three different regions is three on-demand nodes, ~$22/mo —
    over the cap, and the cap would be breached by a dice roll rather than by a
    decision. All-Spot is ~$15.7/mo at three nodes regardless of the draw.

    What used to justify an on-demand base was single-region concentration: one
    Spot capacity event could take the whole pool. Random multi-region placement is
    now itself the diversification — a capacity event is scoped to one region, the
    reconciler self-heals, and the client fails over across the directory. Set this
    to 1 to buy back per-region on-demand anchoring, and raise monthly_budget_usd
    with it. See the README cost table.
  EOT
  type        = number
  default     = 0
}

variable "rotation_interval_hours" {
  description = <<-EOT
    How often the reconciler re-draws the random region placement. Between draws
    the placement is stable (persisted in SSM), so nodes are not churned on every
    2-minute reconcile. A re-draw terminates nodes in regions that lost a slot and
    launches them where the draw sent them — that IS the rotation.
  EOT
  type        = number
  default     = 24
}

# ── Jurisdiction (state-based denylist, §4) ─────────────────────────────────

variable "state_denylist" {
  description = <<-EOT
    US states denied for relay placement. A region is excluded iff the state its
    AZs sit in appears here.

    DEFAULT IS NOW EMPTY — all four US regions are allowed. The original denylist
    (Virginia/Ohio/California, for age-verification and device-level age-assurance
    laws) was placement HYGIENE, not compliance: those laws attach to content
    services and users, never to server racks, so nothing was ever being broken by
    hosting there. It was traded away deliberately for a wider random-placement
    pool — unpredictable, rotating placement across four regions is the property
    being bought. To re-deny, put the state names back here; jurisdiction.tf still
    enforces it mechanically and region_state_map below still records the mapping.
  EOT
  type        = set(string)
  default     = []
}

variable "region_state_map" {
  description = <<-EOT
    AWS region -> US state its AZs sit in, for every region the pool may use. This
    is BOTH the candidate-region set (its keys) and the jurisdiction policy source
    of truth (its values, checked against state_denylist).

    Adding a region here is not enough on its own: each candidate needs a statically
    declared aliased provider and module block in providers.tf/main.tf, because
    Terraform cannot synthesize a provider per region dynamically.
  EOT
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
