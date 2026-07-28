# ── Naming (env-namespaced) ─────────────────────────────────────────────────
# env="prod" reproduces the ORIGINAL names byte-for-byte (so a prod re-apply is a
# no-op); any other env prefixes every named resource so a second isolated pool
# can coexist in the same account+region. Threaded into every module + ssm.tf.
locals {
  is_prod          = var.env == "prod"
  name_prefix      = local.is_prod ? "pollis-relay-hydra" : "pollis-relay-hydra-${var.env}"
  node_name_prefix = local.is_prod ? "pollis-relay" : "pollis-relay-${var.env}"
  # CloudWatch metric namespace: PascalCase, per-env so alarms never cross wires.
  metric_namespace = local.is_prod ? "PollisRelayHydra" : "PollisRelayHydra${title(var.env)}"

  # Regions that have a static aliased provider (providers.tf) AND a module block
  # below. jurisdiction.tf asserts region_state_map is a subset of this, so a
  # region added to the map without the wiring fails the plan instead of silently
  # never being drawn.
  region_providers_wired = {
    "us-east-1" = true
    "us-east-2" = true
    "us-west-1" = true
    "us-west-2" = true
  }
}

# ── Relay nodes: one shard per allowed region ───────────────────────────────
#
# Every allowed region gets a full shard (VPC + SG + IAM + ASG) standing by at
# desired_capacity 0. Idle shards are free — VPCs, security groups, launch
# templates and empty ASGs cost nothing; only running instances bill. The
# reconciler then draws each node's region at random on rotation and moves ASG
# desired capacities to match, so a node can appear in any allowed region without
# a Terraform apply.
#
# These are four near-identical static blocks rather than a for_each because
# Terraform cannot assign a provider dynamically per module instance. Adding a
# region = an entry in region_state_map + an alias in providers.tf + a block here.
#
# DO NOT add `depends_on = [terraform_data.jurisdiction_guard]` here. A module-level
# depends_on defers EVERY data source inside that module to apply time, and when
# allowed_regions changes the guard's output is unknown at plan time — so
# data.aws_availability_zones goes unknown, local.azs with it, and aws_subnet.public
# plans as destroy-then-create on the LIVE region, taking the running nodes' subnets
# with it. The guard needs no ordering edge: its preconditions fail the whole plan on
# their own, which is the actual jurisdiction guarantee.

module "relay_region_us_east_1" {
  source = "./modules/relay-region"
  count  = contains(local.allowed_regions, "us-east-1") ? 1 : 0

  providers = { aws = aws.us_east_1 }

  name_prefix         = local.node_name_prefix
  region              = "us-east-1"
  param_region        = var.primary_region
  node_max            = var.node_max
  on_demand_base      = var.on_demand_base_per_region
  instance_type       = var.instance_type
  spot_max_price      = var.spot_max_price
  relay_image         = var.relay_image
  relay_port          = var.relay_port
  health_port         = var.health_port
  relay_allowlist     = var.relay_allowlist
  identity_key_param  = local.identity_key_param
  identity_cert_param = local.identity_cert_param
}

module "relay_region_us_east_2" {
  source = "./modules/relay-region"
  count  = contains(local.allowed_regions, "us-east-2") ? 1 : 0

  providers = { aws = aws.us_east_2 }

  name_prefix         = local.node_name_prefix
  region              = "us-east-2"
  param_region        = var.primary_region
  node_max            = var.node_max
  on_demand_base      = var.on_demand_base_per_region
  instance_type       = var.instance_type
  spot_max_price      = var.spot_max_price
  relay_image         = var.relay_image
  relay_port          = var.relay_port
  health_port         = var.health_port
  relay_allowlist     = var.relay_allowlist
  identity_key_param  = local.identity_key_param
  identity_cert_param = local.identity_cert_param
}

module "relay_region_us_west_1" {
  source = "./modules/relay-region"
  count  = contains(local.allowed_regions, "us-west-1") ? 1 : 0

  providers = { aws = aws.us_west_1 }

  name_prefix = local.node_name_prefix
  region      = "us-west-1"
  # N. California exposes only two AZs to most accounts; the module's default of
  # three would fail the apply here.
  az_count            = 2
  param_region        = var.primary_region
  node_max            = var.node_max
  on_demand_base      = var.on_demand_base_per_region
  instance_type       = var.instance_type
  spot_max_price      = var.spot_max_price
  relay_image         = var.relay_image
  relay_port          = var.relay_port
  health_port         = var.health_port
  relay_allowlist     = var.relay_allowlist
  identity_key_param  = local.identity_key_param
  identity_cert_param = local.identity_cert_param
}

module "relay_region_us_west_2" {
  source = "./modules/relay-region"
  count  = contains(local.allowed_regions, "us-west-2") ? 1 : 0

  providers = { aws = aws.us_west_2 }

  name_prefix         = local.node_name_prefix
  region              = "us-west-2"
  param_region        = var.primary_region
  node_max            = var.node_max
  on_demand_base      = var.on_demand_base_per_region
  instance_type       = var.instance_type
  spot_max_price      = var.spot_max_price
  relay_image         = var.relay_image
  relay_port          = var.relay_port
  health_port         = var.health_port
  relay_allowlist     = var.relay_allowlist
  identity_key_param  = local.identity_key_param
  identity_cert_param = local.identity_cert_param
}

# The original single-region pool was `module.relay_region` keyed by region, with
# us-west-2 as the only allowed region. Without this the rename would read as
# "destroy the live pool, create a new one" — the VPC, ASG and nodes are the same
# resources, only the module address changed.
moved {
  from = module.relay_region["us-west-2"]
  to   = module.relay_region_us_west_2[0]
}

locals {
  # region -> ASG name, for every shard that actually got created.
  managed_regions = merge(
    { for m in module.relay_region_us_east_1 : "us-east-1" => m.asg_name },
    { for m in module.relay_region_us_east_2 : "us-east-2" => m.asg_name },
    { for m in module.relay_region_us_west_1 : "us-west-1" => m.asg_name },
    { for m in module.relay_region_us_west_2 : "us-west-2" => m.asg_name },
  )
}

# ── Signed-directory hosting: S3 (private) + CloudFront (OAC) ────────────────
module "directory" {
  source = "./modules/directory"

  providers = {
    aws           = aws
    aws.us_east_1 = aws.us_east_1
  }

  name_prefix          = local.name_prefix
  directory_domain     = var.directory_domain
  directory_object_key = var.directory_object_key
}

# ── Reconciler: Lambda + schedule + IAM + alarms ────────────────────────────
module "reconciler" {
  source = "./modules/reconciler"

  primary_region     = var.primary_region
  managed_regions    = local.managed_regions
  reconcile_schedule = var.reconcile_schedule

  desired_state_param     = local.desired_state_param
  desired_state_param_arn = local.desired_state_param_arn
  placement_param         = local.placement_param
  placement_param_arn     = local.placement_param_arn
  signing_key_param       = local.signing_key_param
  identity_cert_param     = local.identity_cert_param
  secret_param_arns       = local.secret_param_arns

  directory_bucket      = module.directory.bucket_name
  directory_bucket_arn  = module.directory.bucket_arn
  directory_object_key  = var.directory_object_key
  directory_ttl_seconds = var.directory_ttl_seconds

  relay_port              = var.relay_port
  health_port             = var.health_port
  node_floor              = var.node_floor
  node_max                = var.node_max
  rotation_interval_hours = var.rotation_interval_hours

  name_prefix      = local.name_prefix
  metric_namespace = local.metric_namespace

  alarm_email_addresses = var.alarm_email_addresses
}

# ── Guardrail: AWS Budgets alert at the §0 hard cap ─────────────────────────
resource "aws_budgets_budget" "monthly_cap" {
  name         = local.name_prefix
  budget_type  = "COST"
  limit_amount = tostring(var.monthly_budget_usd)
  limit_unit   = "USD"
  time_unit    = "MONTHLY"

  # Deliberately UNFILTERED: tracks total account spend so the hard cap can never
  # silently match $0. A tag filter (`cost_filter { name = "TagKeyValue", values =
  # ["user:app$pollis-relay"] }`) only works once `app` is activated as a cost-
  # allocation tag in Billing — add it then if this account hosts more than the pool.

  # Alert at 80% forecasted and 100% actual.
  notification {
    comparison_operator        = "GREATER_THAN"
    threshold                  = 80
    threshold_type             = "PERCENTAGE"
    notification_type          = "FORECASTED"
    subscriber_email_addresses = var.budget_alert_emails
  }

  notification {
    comparison_operator        = "GREATER_THAN"
    threshold                  = 100
    threshold_type             = "PERCENTAGE"
    notification_type          = "ACTUAL"
    subscriber_email_addresses = var.budget_alert_emails
  }
}
