# Signed-directory hosting: a PRIVATE S3 bucket fronted by CloudFront via Origin
# Access Control (OAC). The reconciler writes the signed envelope to S3; clients
# only ever reach it through CloudFront at the stable HTTPS URL. Short cache TTL
# so a re-sign propagates within ~30s.

terraform {
  required_providers {
    aws = {
      source                = "hashicorp/aws"
      configuration_aliases = [aws.us_east_1]
    }
  }
}

locals {
  use_custom_domain = var.directory_domain != ""
}

data "aws_caller_identity" "current" {}

resource "aws_s3_bucket" "directory" {
  bucket        = "${var.name_prefix}-directory-${data.aws_caller_identity.current.account_id}"
  force_destroy = true
  tags          = { app = "pollis-relay" }
}

resource "aws_s3_bucket_public_access_block" "directory" {
  bucket                  = aws_s3_bucket.directory.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_cloudfront_origin_access_control" "directory" {
  name                              = "${var.name_prefix}-directory"
  origin_access_control_origin_type = "s3"
  signing_behavior                  = "always"
  signing_protocol                  = "sigv4"
}

# ── ACM certificate (us-east-1, required for CloudFront) ─────────────────────

resource "aws_acm_certificate" "directory" {
  count             = local.use_custom_domain ? 1 : 0
  provider          = aws.us_east_1
  domain_name       = var.directory_domain
  validation_method = "DNS"

  lifecycle {
    create_before_destroy = true
  }

  tags = { app = "pollis-relay" }
}

# Waits until the DNS validation record (see acm_validation_records output) is
# live. Smooth path: `terraform apply -target=module.directory.aws_acm_certificate.directory`
# first, add the printed CNAME at the pollis.com DNS host, then a full apply.
resource "aws_acm_certificate_validation" "directory" {
  count           = local.use_custom_domain ? 1 : 0
  provider        = aws.us_east_1
  certificate_arn = aws_acm_certificate.directory[0].arn
}

# ── CloudFront distribution ─────────────────────────────────────────────────

resource "aws_cloudfront_distribution" "directory" {
  enabled         = true
  is_ipv6_enabled = true
  comment         = "Pollis relay signed directory"
  price_class     = "PriceClass_100" # cheapest edge footprint (NA + EU)

  aliases = local.use_custom_domain ? [var.directory_domain] : []

  origin {
    domain_name              = aws_s3_bucket.directory.bucket_regional_domain_name
    origin_id                = "directory-s3"
    origin_access_control_id = aws_cloudfront_origin_access_control.directory.id
  }

  default_cache_behavior {
    target_origin_id       = "directory-s3"
    viewer_protocol_policy = "redirect-to-https"
    allowed_methods        = ["GET", "HEAD"]
    cached_methods         = ["GET", "HEAD"]
    compress               = true

    # Short TTLs so a fresh re-sign propagates quickly.
    min_ttl     = 0
    default_ttl = 30
    max_ttl     = 60

    forwarded_values {
      query_string = false
      cookies {
        forward = "none"
      }
    }
  }

  default_root_object = var.directory_object_key

  restrictions {
    geo_restriction {
      restriction_type = "none"
    }
  }

  dynamic "viewer_certificate" {
    for_each = local.use_custom_domain ? [1] : []
    content {
      acm_certificate_arn      = aws_acm_certificate_validation.directory[0].certificate_arn
      ssl_support_method       = "sni-only"
      minimum_protocol_version = "TLSv1.2_2021"
    }
  }

  dynamic "viewer_certificate" {
    for_each = local.use_custom_domain ? [] : [1]
    content {
      cloudfront_default_certificate = true
    }
  }

  tags = { app = "pollis-relay" }
}

# ── Bucket policy: only this CloudFront distribution may read ────────────────

data "aws_iam_policy_document" "bucket" {
  statement {
    sid       = "AllowCloudFrontOAC"
    actions   = ["s3:GetObject"]
    resources = ["${aws_s3_bucket.directory.arn}/*"]
    principals {
      type        = "Service"
      identifiers = ["cloudfront.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "AWS:SourceArn"
      values   = [aws_cloudfront_distribution.directory.arn]
    }
  }
}

resource "aws_s3_bucket_policy" "directory" {
  bucket = aws_s3_bucket.directory.id
  policy = data.aws_iam_policy_document.bucket.json
}
