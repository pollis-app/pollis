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
}

# ── Relay nodes: one module instance per allowed region ─────────────────────
#
# for_each over the jurisdiction-filtered region set. All allowed regions must
# equal primary_region for now (Terraform can't synthesize a provider per region
# dynamically, and §4 leaves us-west-2 as the only clean US region anyway). To
# add a second clean region later: add an aliased provider for it (providers.tf),
# then a second `module "relay_region_<r>"` block passing that provider. The
# module itself is fully region-parameterized, so that is the only edit.
module "relay_region" {
  source   = "./modules/relay-region"
  for_each = toset(local.allowed_regions)

  name_prefix     = local.node_name_prefix
  region          = each.value
  node_floor      = var.node_floor
  node_max        = var.node_max
  instance_type   = var.instance_type
  spot_max_price  = var.spot_max_price
  relay_image     = var.relay_image
  relay_port      = var.relay_port
  health_port     = var.health_port
  relay_allowlist = var.relay_allowlist

  identity_key_param  = local.identity_key_param
  identity_cert_param = local.identity_cert_param

  depends_on = [terraform_data.jurisdiction_guard]
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
  managed_regions    = { for r, m in module.relay_region : r => m.asg_name }
  reconcile_schedule = var.reconcile_schedule

  desired_state_param     = local.desired_state_param
  desired_state_param_arn = local.desired_state_param_arn
  signing_key_param       = local.signing_key_param
  identity_cert_param     = local.identity_cert_param
  secret_param_arns       = local.secret_param_arns

  directory_bucket      = module.directory.bucket_name
  directory_bucket_arn  = module.directory.bucket_arn
  directory_object_key  = var.directory_object_key
  directory_ttl_seconds = var.directory_ttl_seconds

  relay_port  = var.relay_port
  health_port = var.health_port
  node_floor  = var.node_floor
  node_max    = var.node_max

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
