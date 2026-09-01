# ── The two outputs the client build needs (§6) ─────────────────────────────

output "POLLIS_OVERLAY_DIRECTORY_URL" {
  description = "Stable HTTPS URL the client fetches the signed directory from. Bake into the client build."
  value       = module.directory.directory_url
}

output "POLLIS_OVERLAY_REVOCATION_URL" {
  description = "Stable HTTPS URL the client fetches the signed relay-revocation list from (#813). Bake into the client build. Signed by the SAME key as the directory, so there is no third build-time constant."
  value       = module.directory.revocation_url
}

output "POLLIS_OVERLAY_DIRECTORY_KEY" {
  description = "base64 of the 32-byte Ed25519 directory-signing PUBLIC key. Printed by scripts/mint-signing-key.sh; re-surfaced here for convenience if you stored it in SSM."
  value       = "Run scripts/mint-signing-key.sh — it prints this. (Kept out of Terraform so the private half never touches TF state.)"
}

# ── DNS wiring for the custom domain (manual, one-time) ─────────────────────

output "acm_validation_records" {
  description = "DNS records to add (at pollis.com's DNS host) to validate the ACM certificate. Empty when directory_domain is \"\"."
  value       = module.directory.acm_validation_records
}

output "directory_cname_target" {
  description = "Add a CNAME: directory_domain -> this CloudFront domain (at pollis.com's DNS host)."
  value       = module.directory.cloudfront_domain
}

# ── Operational handles ─────────────────────────────────────────────────────

output "directory_bucket" {
  description = "S3 bucket the reconciler publishes the signed directory to."
  value       = module.directory.bucket_name
}

output "hydra_enabled" {
  description = "THE OFF SWITCH (var.hydra_enabled). false = the pool is switched off: no per-region ASG, no reconciler, no schedule, nothing running or billing beyond the (near-free) directory hosting and the SSM parameters. Flip the variable's default in variables.tf and re-apply to bring it back."
  value       = var.hydra_enabled
}

output "reconciler_function_name" {
  description = "Invoke on-demand with: aws lambda invoke --function-name <this> /dev/stdout. Empty while the pool is switched off — there is no reconciler then."
  value       = one(module.reconciler[*].function_name)
}

output "desired_state_param" {
  description = "Edit this SSM param to scale the pool: aws ssm put-parameter --name <this> --type String --overwrite --value '{\"total\":3}'. Regions are NOT set here — the reconciler draws them."
  value       = aws_ssm_parameter.desired_state.name
}

output "placement_param" {
  description = "Read this SSM param to see where the current random draw put the pool, and when it was drawn: aws ssm get-parameter --name <this> --query Parameter.Value --output text"
  value       = aws_ssm_parameter.placement.name
}

output "intended_image_param" {
  description = "SSM param recording the intended relay build ({image, sha}). CI writes it on every roll; the reconciler and the nodes' user-data read it. Seed a pinned digest by hand for a fresh pool: aws ssm put-parameter --name <this> --type String --overwrite --value '{\"image\":\"ghcr.io/actuallydan/pollis-relay@sha256:...\",\"sha\":\"<gitsha>\"}'."
  value       = aws_ssm_parameter.intended_image.name
}

output "revocations_param" {
  description = "SSM param holding the live relay-revocation set (#813). Revoke a node NOW with: aws ssm put-parameter --name <this> --type String --overwrite --value '{\"revoked\":[{\"ip\":\"203.0.113.7\",\"reason\":\"seized\"}]}'. Its SSM Version is the published sequence number — NEVER delete this parameter (that resets Version to 1 and every client rejects it as a rollback); write an empty array to clear."
  value       = aws_ssm_parameter.revocations.name
}

output "relay_image_oidc_role_arn" {
  description = "ARN of the GitHub-OIDC role relay-image.yml assumes to record the intended build. Add it to the repo as the RELAY_IMAGE_OIDC_ROLE_ARN Actions VARIABLE (not a secret). Empty when github_repository is unset (record the param by hand instead)."
  value       = local.ci_oidc_enabled ? aws_iam_role.ci_record_image[0].arn : ""
}

output "allowed_regions" {
  description = "Regions that passed the §4 jurisdiction filter. The reconciler draws node placement from exactly this set — WHEN the pool is switched on. This is the jurisdiction answer, not the deployment one: see active_regions."
  value       = local.allowed_regions
}

output "active_regions" {
  description = "Allowed regions that actually hold a shard right now. Empty while hydra_enabled is false."
  value       = local.active_regions
}

output "asg_names" {
  description = "Per-region Auto Scaling Group names. Every active region has one, standing by at desired capacity 0 until a draw sends nodes there. Empty while the pool is switched off."
  value       = local.managed_regions
}
