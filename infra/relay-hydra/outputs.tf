# ── The two outputs the client build needs (§6) ─────────────────────────────

output "POLLIS_OVERLAY_DIRECTORY_URL" {
  description = "Stable HTTPS URL the client fetches the signed directory from. Bake into the client build."
  value       = module.directory.directory_url
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

output "reconciler_function_name" {
  description = "Invoke on-demand with: aws lambda invoke --function-name <this> /dev/stdout"
  value       = module.reconciler.function_name
}

output "desired_state_param" {
  description = "Edit this SSM param to scale the pool: aws ssm put-parameter --name <this> --type String --overwrite --value '{\"total\":3}'. Regions are NOT set here — the reconciler draws them."
  value       = aws_ssm_parameter.desired_state.name
}

output "placement_param" {
  description = "Read this SSM param to see where the current random draw put the pool, and when it was drawn: aws ssm get-parameter --name <this> --query Parameter.Value --output text"
  value       = aws_ssm_parameter.placement.name
}

output "allowed_regions" {
  description = "Regions that passed the §4 jurisdiction filter. The reconciler draws node placement from exactly this set."
  value       = local.allowed_regions
}

output "asg_names" {
  description = "Per-region Auto Scaling Group names. Every allowed region has one, standing by at desired capacity 0 until a draw sends nodes there."
  value       = local.managed_regions
}
