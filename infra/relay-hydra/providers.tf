# The default provider is the CONTROL PLANE region (primary_region): reconciler
# Lambda, directory bucket, every SSM parameter. Relay nodes are placed across the
# aliased per-region providers below.
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

# ── One aliased provider per candidate region (§4 / jurisdiction.tf) ─────────
#
# Terraform cannot synthesize a provider per element of a variable, so every
# region the pool may ever draw needs a static alias here and a matching module
# block in main.tf. Adding a region is therefore a three-line change: an entry in
# region_state_map, an alias here, a module block there. jurisdiction.tf fails the
# plan if a region is added to the map without this wiring.
#
# us_east_1 does double duty: CloudFront's ACM certificate MUST be minted in
# us-east-1 regardless of where the rest of the stack runs, and the directory
# module consumes this same alias for that.

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

provider "aws" {
  alias  = "us_east_2"
  region = "us-east-2"

  default_tags {
    tags = {
      app       = "pollis-relay"
      component = "relay-hydra"
      managed   = "terraform"
    }
  }
}

provider "aws" {
  alias  = "us_west_1"
  region = "us-west-1"

  default_tags {
    tags = {
      app       = "pollis-relay"
      component = "relay-hydra"
      managed   = "terraform"
    }
  }
}

provider "aws" {
  alias  = "us_west_2"
  region = "us-west-2"

  default_tags {
    tags = {
      app       = "pollis-relay"
      component = "relay-hydra"
      managed   = "terraform"
    }
  }
}
