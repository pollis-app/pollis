#!/usr/bin/env bash
#
# Operator removal of a stored file (#2109). Operator only.
#
#   scripts/remove-stored-file.sh --dry-run <object id>   show what it would remove
#   scripts/remove-stored-file.sh <object id>             remove it
#
# A file is stored once, so removing it takes it from every message and Vault
# entry that holds it. The id is recorded first and refused from then on, so
# the identical file cannot be stored or sent again; apps show the attachment
# as removed. It runs with the Delivery Service's own configuration (its
# database and R2 credentials, from Doppler) through
# `pollis-delivery/examples/remove_stored_file.rs`, which calls the same
# `messages::remove_stored_object` the DS tests cover. No route, no token.
#
# Targets prod by default; DOPPLER_CONFIG=dev_personal for the dev DS.

set -euo pipefail

CONFIG="${DOPPLER_CONFIG:-prd_prod}"
[ $# -ge 1 ] || { sed -n '3,6p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }

cd "$(dirname "$0")/.."
echo "Removing from the DS on Doppler config $CONFIG."
doppler run -p pollis -c "$CONFIG" -- \
  cargo run -q -p pollis-delivery --example remove_stored_file -- "$@"
