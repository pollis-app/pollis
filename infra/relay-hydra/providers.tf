# The pool lives in us-west-2 (Oregon) — the only US region whose state is clean
# under the §4 jurisdiction denylist. See jurisdiction.tf for the enforcement.
provider "aws" {
  region = var.primary_region

  default_tags {
    tags = {
      app       = "pollis-relay"
      component = "relay-hydra"
      managed   = "terraform"
    }
  }
}

# CloudFront's ACM certificate MUST live in us-east-1 regardless of where the
# rest of the stack runs. This aliased provider exists only to mint that cert.
provider "aws" {
  alias  = "us_east_1"
  region = "us-east-1"

  default_tags {
    tags = {
      app       = "pollis-relay"
      component = "relay-hydra"
      managed   = "terraform"
    }
  }
}
