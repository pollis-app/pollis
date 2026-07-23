output "bucket_name" {
  value = aws_s3_bucket.directory.id
}

output "bucket_arn" {
  value = aws_s3_bucket.directory.arn
}

output "cloudfront_domain" {
  description = "CloudFront distribution domain. CNAME the custom domain to this."
  value       = aws_cloudfront_distribution.directory.domain_name
}

output "directory_url" {
  description = "Stable HTTPS URL of the signed directory (POLLIS_OVERLAY_DIRECTORY_URL)."
  value       = local.use_custom_domain ? "https://${var.directory_domain}/${var.directory_object_key}" : "https://${aws_cloudfront_distribution.directory.domain_name}/${var.directory_object_key}"
}

output "acm_validation_records" {
  description = "DNS CNAME(s) to add to validate the ACM certificate. Empty when no custom domain."
  value = local.use_custom_domain ? [
    for o in aws_acm_certificate.directory[0].domain_validation_options : {
      name  = o.resource_record_name
      type  = o.resource_record_type
      value = o.resource_record_value
    }
  ] : []
}
