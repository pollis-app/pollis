variable "name_prefix" {
  description = "Prefix for bucket/distribution naming."
  type        = string
}

variable "directory_domain" {
  description = "Custom domain for the directory (e.g. relays.pollis.com). Empty = use the raw *.cloudfront.net domain."
  type        = string
}

variable "directory_object_key" {
  description = "S3 object key the signed directory is written to (and the URL path)."
  type        = string
}
