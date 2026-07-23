# §4 Jurisdiction enforcement — default-deny by US state.
#
# The jurisdiction unit is the US STATE, not the AWS region: a region is denied
# only because the state its AZs sit in is on the denylist. The deny criterion is
# "any state with an age-verification or a device/OS-level age-registration law."
# As of mid-2026 that denies Virginia (us-east-1), Ohio (us-east-2), and
# California (us-west-1) — leaving Oregon (us-west-2) as the only clean US region.
#
# To add/remove a region later: edit region_state_map (variables.tf), re-check the
# state-law landscape, and — if the state is clean — it becomes selectable. This
# file mechanically refuses to place a node in any denied or unmapped state.

locals {
  requested_regions = keys(var.region_node_counts)

  region_state = { for r in local.requested_regions : r => lookup(var.region_state_map, r, "UNMAPPED") }

  denied_requested = [
    for r in local.requested_regions : r
    if contains(var.state_denylist, local.region_state[r]) || local.region_state[r] == "UNMAPPED"
  ]

  allowed_regions = [
    for r in local.requested_regions : r
    if !contains(var.state_denylist, local.region_state[r]) && local.region_state[r] != "UNMAPPED"
  ]

  primary_region_state = lookup(var.region_state_map, var.primary_region, "UNMAPPED")
}

# Fail `plan`/`apply` hard if any requested region maps to a denied/unmapped state.
resource "terraform_data" "jurisdiction_guard" {
  input = local.allowed_regions

  lifecycle {
    precondition {
      condition     = length(local.denied_requested) == 0
      error_message = "Jurisdiction denylist violation — these requested regions map to a denied or unmapped US state and must not host relays: ${join(", ", [for r in local.denied_requested : "${r} (${local.region_state[r]})"])}. Fix region_node_counts or region_state_map."
    }

    precondition {
      condition     = !contains(var.state_denylist, local.primary_region_state) && local.primary_region_state != "UNMAPPED"
      error_message = "primary_region ${var.primary_region} maps to denied/unmapped state ${local.primary_region_state}."
    }

    # Every allowed region must equal primary_region until multi-provider wiring
    # is added (Terraform can't synthesize a provider per region dynamically).
    # See the module "relay_region" comment in main.tf and the README.
    precondition {
      condition     = alltrue([for r in local.allowed_regions : r == var.primary_region])
      error_message = "Multi-region expansion needs an aliased provider. Regions other than primary_region (${var.primary_region}) present: ${join(", ", [for r in local.allowed_regions : r if r != var.primary_region])}."
    }
  }
}
