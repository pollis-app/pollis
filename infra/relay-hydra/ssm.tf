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

  signing_key_param   = "${local.param_prefix}/signing-key"   # SecureString: Ed25519 private PKCS8 PEM
  identity_key_param  = "${local.param_prefix}/identity-key"  # SecureString: base64(raw) QUIC identity key
  identity_cert_param = "${local.param_prefix}/identity-cert" # SecureString: base64(DER) QUIC leaf cert
  desired_state_param = "${local.param_prefix}/desired-state" # String: {region: count}

  # Conventional ARNs (the params exist by name; no data-source dependency so
  # `plan` works before the mint scripts have run).
  param_arn_prefix = "arn:aws:ssm:${var.primary_region}:${data.aws_caller_identity.current.account_id}:parameter"
  secret_param_arns = [
    "${local.param_arn_prefix}${local.signing_key_param}",
    "${local.param_arn_prefix}${local.identity_key_param}",
    "${local.param_arn_prefix}${local.identity_cert_param}",
  ]
  desired_state_param_arn = "${local.param_arn_prefix}${local.desired_state_param}"
}

# Desired-state IS Terraform-managed (non-secret) and seeded from the input map.
# After apply the reconciler and human operators own the value; Terraform ignores
# drift so scaling edits (aws ssm put-parameter --overwrite) persist across applies.
resource "aws_ssm_parameter" "desired_state" {
  name  = local.desired_state_param
  type  = "String"
  value = jsonencode(var.region_node_counts)

  tags = { app = "pollis-relay" }

  lifecycle {
    ignore_changes = [value]
  }
}
