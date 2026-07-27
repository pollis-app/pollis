# §4 Jurisdiction enforcement — a state denylist over the candidate regions.
#
# The jurisdiction unit is the US STATE, not the AWS region: a region is denied
# only because the state its AZs sit in is on the denylist. `region_state_map` is
# both the candidate set (its keys) and the region->state mapping; `state_denylist`
# is the policy.
#
# The denylist now defaults to EMPTY, so all four US regions are allowed and the
# reconciler draws placement across them at random. The original list (Virginia /
# Ohio / California, for age-verification and device-level age-assurance laws) was
# placement hygiene rather than compliance — see the state_denylist description in
# variables.tf for why it was opened up, and how to close it again.
#
# This file still mechanically refuses to place a node in a denied state, so
# re-denying is a one-line edit that Terraform then enforces on the next apply.

locals {
  candidate_regions = keys(var.region_state_map)

  denied_regions = [
    for r in local.candidate_regions : r
    if contains(var.state_denylist, var.region_state_map[r])
  ]

  # The region set the reconciler may draw placement from.
  allowed_regions = sort([
    for r in local.candidate_regions : r
    if !contains(var.state_denylist, var.region_state_map[r])
  ])

  primary_region_state = lookup(var.region_state_map, var.primary_region, "UNMAPPED")
}

resource "terraform_data" "jurisdiction_guard" {
  input = local.allowed_regions

  lifecycle {
    # An empty allowed set would leave the reconciler with nowhere to place nodes,
    # and it would publish no directory at all.
    precondition {
      condition     = length(local.allowed_regions) > 0
      error_message = "state_denylist denies every candidate region (${join(", ", local.denied_regions)}). At least one region in region_state_map must be allowed."
    }

    # The control plane (reconciler Lambda, directory bucket, all SSM params) sits
    # in primary_region and sees every relay's IP, so it is subject to the same
    # jurisdiction policy as the nodes.
    precondition {
      condition     = !contains(var.state_denylist, local.primary_region_state) && local.primary_region_state != "UNMAPPED"
      error_message = "primary_region ${var.primary_region} maps to denied/unmapped state ${local.primary_region_state}. It must be an allowed region — the control plane sees relay IPs."
    }

    # Each candidate region needs a statically declared aliased provider + module
    # block (Terraform can't synthesize providers dynamically). This catches a
    # region added to region_state_map without the matching wiring in main.tf.
    precondition {
      condition     = length(setsubtract(toset(local.candidate_regions), toset(keys(local.region_providers_wired)))) == 0
      error_message = "region_state_map contains regions with no provider/module wiring in providers.tf + main.tf: ${join(", ", setsubtract(toset(local.candidate_regions), toset(keys(local.region_providers_wired))))}. Add an aliased provider and a module block for each."
    }
  }
}
