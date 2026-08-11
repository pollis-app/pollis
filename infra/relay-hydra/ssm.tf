# SSM Parameter Store — the free (standard-tier) home for the pool's secrets and
# desired-state. Secrets Manager buys nothing here and costs $0.40/secret/mo.
#
# The SECRET parameters (signing private key, QUIC identity key + cert) are NOT
# Terraform-managed resources on purpose: their plaintext must never land in
# Terraform state. They are created out-of-band by scripts/mint-signing-key.sh
# and scripts/mint-relay-identity.sh (which also print the public outputs and can
# run BEFORE apply — see §9 sequencing in the README). Terraform only references
# them by their conventional ARNs (constructed below) for least-privilege IAM,
# and the teardown script deletes them.

data "aws_caller_identity" "current" {}

locals {
  # env-namespaced (see the `name_prefix`/`is_prod` locals in main.tf): prod keeps
  # "/pollis/relay-hydra", a test env gets "/pollis/relay-hydra-test", so the mint
  # scripts + Terraform for the two envs never share secrets.
  param_prefix = local.is_prod ? "/pollis/relay-hydra" : "/pollis/relay-hydra-${var.env}"

  signing_key_param    = "${local.param_prefix}/signing-key"    # SecureString: Ed25519 private PKCS8 PEM
  identity_key_param   = "${local.param_prefix}/identity-key"   # SecureString: base64(raw) QUIC identity key
  identity_cert_param  = "${local.param_prefix}/identity-cert"  # SecureString: base64(DER) QUIC leaf cert
  desired_state_param  = "${local.param_prefix}/desired-state"  # String: {"total": N}
  placement_param      = "${local.param_prefix}/placement"      # String: the reconciler's current random draw
  intended_image_param = "${local.param_prefix}/intended-image" # String: {"image": "<immutable ref>", "sha": "<git sha>"}
  revocations_param    = "${local.param_prefix}/revocations"    # String: {"revoked": [{addr|ip|cert_sha256_b64, reason?}, ...]}

  # Conventional ARNs (the params exist by name; no data-source dependency so
  # `plan` works before the mint scripts have run).
  param_arn_prefix = "arn:aws:ssm:${var.primary_region}:${data.aws_caller_identity.current.account_id}:parameter"
  secret_param_arns = [
    "${local.param_arn_prefix}${local.signing_key_param}",
    "${local.param_arn_prefix}${local.identity_key_param}",
    "${local.param_arn_prefix}${local.identity_cert_param}",
  ]
  desired_state_param_arn  = "${local.param_arn_prefix}${local.desired_state_param}"
  placement_param_arn      = "${local.param_arn_prefix}${local.placement_param}"
  intended_image_param_arn = "${local.param_arn_prefix}${local.intended_image_param}"
  revocations_param_arn    = "${local.param_arn_prefix}${local.revocations_param}"
}

# Desired-state IS Terraform-managed (non-secret) and seeded from pool_node_count.
# After apply the reconciler and human operators own the value; Terraform ignores
# drift so scaling edits (aws ssm put-parameter --overwrite) persist across applies.
#
# NOTE for the existing prod pool: because of ignore_changes this apply does NOT
# rewrite the live value, which is still the pre-multi-region {"us-west-2": 3}.
# The reconciler reads that legacy shape and sums it into a pool total, so the
# upgrade needs no manual SSM edit — see readDesiredTotal() in reconciler/placement.mjs.
resource "aws_ssm_parameter" "desired_state" {
  name  = local.desired_state_param
  type  = "String"
  value = jsonencode({ total = var.pool_node_count })

  tags = { app = "pollis-relay" }

  lifecycle {
    ignore_changes = [value]
  }
}

# The current random region draw, owned entirely by the reconciler at runtime.
# Terraform only creates it (so IAM can reference a parameter that exists) and
# then never touches the value. Persisting the draw is what makes placement STABLE
# between rotations: without it, every 2-minute reconcile would re-randomize and
# churn the whole pool continuously.
resource "aws_ssm_parameter" "placement" {
  name  = local.placement_param
  type  = "String"
  value = jsonencode({ drawn_at = 0, placement = {} })

  tags = { app = "pollis-relay" }

  lifecycle {
    ignore_changes = [value]
  }
}

# The intended relay BUILD, written by CI on every image roll (relay-image.yml)
# and read by BOTH the reconciler (to positively identify build-stale nodes) and
# every node's user-data (to launch the intended, immutable image — no `:latest`
# anywhere in the launch path). Terraform only SEEDS it once, from var.relay_image,
# then never touches the value again (ignore_changes), exactly like `placement` and
# `desired_state`: after apply, CI owns it. Seeding from an empty relay_image writes
# an empty record, which the reconciler and user-data both treat as "not recorded
# yet" — the operator/CI must record a real immutable ref before nodes can launch
# (see the runbook). The value shape is {"image": "<immutable ref>", "sha": "<git
# sha>"}: user-data reads .image, the reconciler reads .sha.
# LIVE RELAY REVOCATION (#813). The operator-authored set of relays that must
# stop being trusted RIGHT NOW, rather than whenever the signed directory's ~1h
# TTL lapses. The reconciler reads it every cycle, drops those relays from the
# directory, destroys the matching nodes, and signs the short-lived
# revocations.json that clients and relays enforce.
#
# THE PARAMETER VERSION IS THE PUBLISHED SEQUENCE NUMBER. SSM increments Version
# server-side on every PutParameter, so the monotonic counter clients use for
# rollback protection is free, cannot collide, and cannot be forgotten by the
# operator. Two consequences, both important:
#
#   1. NEVER DELETE THIS PARAMETER. Recreating it resets Version to 1, which every
#      client that has seen a higher sequence will reject as a rollback — the pool
#      fails closed until the counter climbs back past the old high-water mark.
#      To clear a revocation, write {"revoked": []}; do not delete.
#   2. Terraform SEEDS it empty once, then never touches the value again
#      (ignore_changes), exactly like placement/desired_state/intended_image — so
#      an apply can never silently un-revoke a relay, and operator writes are not
#      clobbered.
resource "aws_ssm_parameter" "revocations" {
  name  = local.revocations_param
  type  = "String"
  value = jsonencode({ revoked = [] })

  tags = { app = "pollis-relay" }

  lifecycle {
    ignore_changes = [value]
  }
}

resource "aws_ssm_parameter" "intended_image" {
  name = local.intended_image_param
  type = "String"
  # A digest ref has no separate git sha here at seed time, so record the ref as
  # both when seeding by hand; CI writes the precise {image, sha} pair thereafter.
  value = jsonencode({ image = var.relay_image, sha = "" })

  tags = { app = "pollis-relay" }

  lifecycle {
    ignore_changes = [value]
  }
}
