# The hydra — automated AWS relay-pool orchestrator (#616)

IaC + reconciler + signed directory for the Pollis closed-overlay relay pool.
This is the **AWS-hosting half**; the relay binary, its image, the client, and the
client's directory-fetch are the monorepo/#455 workstream. The only coupling is
the **§3 signed-directory contract** (`lib/directory-verify.mjs`), proven byte-
exact by `test/directory-contract.test.mjs`.

```
Terraform ──> per-region VPC + locked SG + mixed-instances ASG (t4g.nano, Spot), one per
              ALLOWED region, all standing by at desired capacity 0
          ──> S3 (private) + CloudFront (OAC)  ── serves the signed directory
          ──> reconciler Lambda (EventBridge every 2 min) ── draws random placement, scales the
                                                              ASGs, health-checks /version,
                                                              signs + publishes the directory
          ──> Budgets $20 alert + CloudWatch alarms
SSM (free) ── signing private key · pool QUIC identity (SecureString) · desired-state · placement
```

There is **no load balancer**: clients fetch the signed directory and do their own
health/failover. Each relay is just a node with a public UDP port.

**Placement is random and rotates.** Desired-state is a pool-wide *count*, not a
per-region map: every `rotation_interval_hours` (default 24) the reconciler draws
each node's region uniformly at random from the allowed set, persists the draw to
SSM, and converges the ASGs to it. Draws sample **with replacement**, so two nodes
landing in the same region is expected, not a bug. Nothing in the client needs to
change when a node moves — the whole pool shares one pinned QUIC identity, so the
client pins the cert, never the address.

## What you hand back to the client build (§6 outputs)

| Output | Where it comes from |
| --- | --- |
| `POLLIS_OVERLAY_DIRECTORY_URL` | `terraform output POLLIS_OVERLAY_DIRECTORY_URL` (default `https://relays.pollis.com/directory.json`) |
| `POLLIS_OVERLAY_DIRECTORY_KEY` | printed by `scripts/mint-signing-key.sh` (base64 of the 32-byte Ed25519 public key) |

---

## Prerequisites

1. **AWS auth.** `aws login` (interactive), then `aws sts get-caller-identity` must
   succeed. The account needs VPC/EC2/ASG/IAM/SSM/S3/CloudFront/Lambda/EventBridge/
   Budgets/CloudWatch.

   > **Terraform can't see an `aws login` session.** That flow stores short-lived
   > creds under `~/.aws/login/`, which the AWS CLI resolves but the Go SDK the AWS
   > provider uses does not — `terraform plan` fails with "No valid credential
   > sources found". Export them into the environment first, in the same shell:
   >
   > ```bash
   > eval "$(aws configure export-credentials --format env)"
   > ```
   >
   > These expire (check `AWS_CREDENTIAL_EXPIRATION`, typically a few hours). Re-run
   > `aws login` + the `eval` when they do. Don't start a long apply — the CloudFront
   > distribution alone takes several minutes — with only minutes left on the clock.
2. **The relay image is published and pullable by the nodes.** Run
   `.github/workflows/relay-image.yml` (needs org `packages: write`) and make
   `ghcr.io/actuallydan/pollis-relay` **public** (or add a pull secret to the
   user-data). This is a prerequisite, not part of the Terraform.
3. **Terraform ≥ 1.6** and Node ≥ 20 (for the scripts/test).

   > **State is local and gitignored** (`terraform.tfstate` next to this README).
   > Losing it orphans every resource below — they keep billing and nothing manages
   > them. Run applies from a durable checkout, not a temp dir, and back the file up
   > (or move to an S3 backend) before the pool grows.
4. **Allowlist hostnames** — the defaults in `variables.tf` were pulled from
   `.env.production`; re-verify against the current file before apply.

## First-run sequence

```bash
cd infra/relay-hydra

# 1. Mint the directory signing key FIRST (§9). Prints POLLIS_OVERLAY_DIRECTORY_KEY —
#    hand it to the client build so it proceeds in parallel. Stores the private
#    key in SSM. Safe to run before apply.
scripts/mint-signing-key.sh us-west-2

# 2. Mint the ONE shared pool QUIC identity → SSM (key + cert). Prints cert_b64.
scripts/mint-relay-identity.sh us-west-2

# 3. CAA PRE-FLIGHT — do this BEFORE minting the cert (see the warning below).
dig +short CAA relays.pollis.com; dig +short CAA pollis.com
#    → if any CAA records exist and none names an Amazon CA, add one scoped to the
#      subdomain first:  relays.pollis.com  CAA  0 issue "amazon.com"

# 4. Custom domain: create the ACM cert first so you can add its DNS validation
#    record, then a full apply once the cert is issued.
cp terraform.tfvars.example terraform.tfvars    # edit as needed
terraform init
terraform apply -target=module.directory.aws_acm_certificate.directory
#    → add the CNAME from `terraform output acm_validation_records` at pollis.com's
#      DNS host (Cloudflare). Wait until ACM shows "Issued" (minutes).

# 5. Full apply.
terraform apply
#    → add a CNAME:  relays.pollis.com  →  <terraform output directory_cname_target>

# 6. Prove the contract end to end against the live URL.
node scripts/verify-directory.mjs "$(terraform output -raw POLLIS_OVERLAY_DIRECTORY_URL)" "<POLLIS_OVERLAY_DIRECTORY_KEY>"
```

> Using the raw CloudFront domain instead? Set `directory_domain = ""`, skip steps
> 3–4 (the CAA check, the `-target`, and the CNAMEs) and just `terraform apply`.

> ### ⚠️ CAA will fail the cert if you skip step 3
> ACM issues from Amazon Trust Services. If the domain publishes **any** CAA
> records, at least one must name an Amazon CA (`amazon.com` is the documented
> minimum; `amazontrust.com` / `awstrust.com` / `amazonaws.com` also count) or
> issuance is forbidden. `pollis.com` carries a CAA allowlist (Comodo, DigiCert,
> Let's Encrypt, Google, Sectigo, SSL.com) that **excludes Amazon**, so the first
> attempt here failed with `FailureReason: CAA_ERROR` — with a perfectly correct
> validation CNAME in place, which makes it look like a DNS problem when it isn't.
>
> A `FAILED` ACM certificate **cannot be retried** — fix the CAA record, then
> replace the cert:
> ```bash
> terraform apply -replace='module.directory.aws_acm_certificate.directory[0]' \
>                 -target=module.directory.aws_acm_certificate.directory
> ```
> ACM reuses the same validation token per domain+account, so the existing
> validation CNAME stays valid across the replacement — don't re-add it. The CAA
> record is scoped to `relays.pollis.com` on purpose: it lets Amazon issue for that
> one name without loosening the apex policy for the rest of pollis.com.

---

## Runbook

### Scale the pool (set desired-state)
Desired-state is a **pool-wide count**; the reconciler picks the regions. It reads
the param from SSM and converges within one cycle (~2 min). Terraform seeds it once,
then leaves it alone.
```bash
aws ssm put-parameter --region us-west-2 --overwrite \
  --name /pollis/relay-hydra/desired-state --type String \
  --value '{"total": 3}'
# force an immediate reconcile instead of waiting for the schedule:
aws lambda invoke --function-name "$(terraform output -raw reconciler_function_name)" /dev/stdout
```
Counts are clamped to `[node_floor, node_max]`. To raise the ceiling, bump
`node_max` in tfvars and re-apply (mind the $20 cap — see cost below).

> The pre-multi-region per-region shape (`{"us-west-2": 3}`) is still accepted and
> summed into a pool total, so the live param did not need editing during the
> upgrade. Write the `{"total": N}` shape for anything new.

### See / force the random placement
```bash
# where the current draw put the pool, and when it was drawn:
aws ssm get-parameter --region us-west-2 \
  --name "$(terraform output -raw placement_param)" --query Parameter.Value --output text
# → {"drawn_at":1753574400,"placement":{"us-east-2":2,"us-west-1":1}}
```
The reconciler re-draws when the interval has elapsed, when the pool size changed,
or **immediately** when a region leaves the allowed set (a tightened denylist moves
nodes now, not at the next scheduled rotation). To force a rotation early, zero the
`drawn_at` and invoke:
```bash
aws ssm put-parameter --region us-west-2 --overwrite --type String \
  --name "$(terraform output -raw placement_param)" --value '{"drawn_at":0,"placement":{}}'
aws lambda invoke --function-name "$(terraform output -raw reconciler_function_name)" /dev/stdout
```
A rotation terminates nodes in regions that lost a slot and launches replacements
where the draw sent them, so expect a brief dip in healthy nodes — the client
fails over across the directory while it happens.

### Add / remove a region
1. Confirm the region's US state is acceptable under `state_denylist` (currently
   empty — see Jurisdiction below) and present in `region_state_map` (variables.tf).
2. Add an **aliased provider** for the region in `providers.tf`, and a matching
   `module "relay_region_<r>"` block passing that provider (the module is fully
   region-parameterized — that's the only code edit). Terraform cannot synthesize a
   provider per `for_each` element, which is why these are static blocks.
3. Apply. The new region's ASG stands by at capacity 0 and starts receiving nodes at
   the next draw. To **remove** one, add its state to `state_denylist` and apply: the
   next reconcile re-draws immediately and drains it.

`jurisdiction.tf` **fails the plan** if a candidate region maps to a denied or
unmapped state, or if it lacks provider/module wiring — the allowed set and the set
the reconciler can actually drive are kept identical by construction.

### Rotate the directory signing key
Coordinated with a client rebuild (the client pins the public key).
```bash
scripts/mint-signing-key.sh us-west-2      # overwrites the SSM private key, prints the new public key
# → ship a client build with the new POLLIS_OVERLAY_DIRECTORY_KEY, then let the
#   reconciler re-sign. Old directories fail closed once they expire (≤1h).
```

### Rotate the pool QUIC identity
Also a coordinated client rebuild (the client pins the leaf cert).
```bash
scripts/mint-relay-identity.sh us-west-2   # overwrites the SSM identity key + cert
# → roll the nodes so they refetch (terminate them; the ASG relaunches), and ship
#   a client build with the new pinned cert. The reconciler puts the new cert_b64
#   in the next directory automatically.
```

### Stand up a throwaway TEST env (`env=test`)

Exercise the REAL infra end to end — a dev-enrolled client through real AWS
Graviton relays to your dev services — without touching prod and without a prod
login. `env` namespaces every named resource (S3 bucket, SSM params, Lambda, ASG,
IAM roles, SG, alarms, budget), so a second isolated pool coexists in the SAME
account+region. `env=prod` (the default) reproduces the original names exactly, so
prod is never affected. Cost is a few cents for a short session (see Cost below).

```bash
# From a SEPARATE working dir so the test state never touches prod state.
cd infra/relay-hydra          # (a fresh checkout / worktree — its own terraform.tfstate)

# Mint TEST signing key + identity into the test SSM prefix (note the param names):
scripts/mint-signing-key.sh   us-west-2 /pollis/relay-hydra-test/signing-key
scripts/mint-relay-identity.sh us-west-2   # writes /pollis/relay-hydra-test/* if you pass the test names

# Point the pool at your DEV hosts and keep it to one node.
cat > test.tfvars <<'EOF'
env                   = "test"
directory_domain      = ""          # raw *.cloudfront.net — skips ACM/DNS/CAA
relay_allowlist       = "*.turso.io,*.pollis.com,*.cloudflarestorage.com"
pool_node_count       = 1
node_floor            = 1
node_max              = 1
# Pin a test pool to one region so you know where to look; prod draws at random.
state_denylist        = ["Virginia", "Ohio", "California"]
alarm_email_addresses = []
EOF
terraform init
terraform apply -var-file=test.tfvars

# Hand the two outputs to a DEV client build (raw CloudFront URL, no DNS):
terraform output -raw POLLIS_OVERLAY_DIRECTORY_URL   # https://<id>.cloudfront.net/directory.json
# POLLIS_OVERLAY_DIRECTORY_KEY = the public key printed by mint-signing-key.sh above

# ...run the client (DEV_EMAIL auto-login) in Strict mode against those, then:
terraform destroy -var-file=test.tfvars
aws ssm delete-parameters --region us-west-2 --names \
  /pollis/relay-hydra-test/signing-key /pollis/relay-hydra-test/identity-key \
  /pollis/relay-hydra-test/identity-cert /pollis/relay-hydra-test/desired-state
```

### Tear it all down
```bash
terraform destroy                 # add -var-file=test.tfvars for a test env
# The signing/identity SSM SecureStrings are NOT Terraform-managed (their plaintext
# must never touch TF state) — delete them explicitly (swap the -test prefix for a
# test env):
aws ssm delete-parameters --region us-west-2 --names \
  /pollis/relay-hydra/signing-key /pollis/relay-hydra/identity-key \
  /pollis/relay-hydra/identity-cert /pollis/relay-hydra/desired-state
```

---

## Cost (§0 hard target: < $20/month)

Per-node, all-in (us-west-2 pricing; the other US regions are within a few cents):

| Item | On-demand node | Spot node |
| --- | --- | --- |
| `t4g.nano` compute (730 h) | ~$3.07 | ~$0.95 |
| Public IPv4 (the dominant cost) | ~$3.65 | ~$3.65 |
| 8 GiB gp3 EBS | ~$0.64 | ~$0.64 |
| **Per node** | **~$7.36** | **~$5.24** |

Default config = `pool_node_count = 3`, `node_floor = 2`, `node_max = 3`, and
`on_demand_base_per_region = 0` (all Spot):

- **Steady state (floor, 2 nodes): ~$10.5/mo.**
- **Full pool (3 nodes): ~$15.7/mo** — under the cap *whatever the draw does*.
- Lambda + S3 + CloudFront + SSM (standard tier, free) + EventBridge + a handful of
  CloudWatch alarms ($0.10 each): **< $1/mo**.

**Why all Spot now.** `on_demand_base_per_region` is per-REGION, so its cost scales
with how wide the random draw spreads: at `1`, a 3-node pool that lands in three
different regions is three on-demand nodes, **~$22/mo — over the §0 cap, breached by
a dice roll rather than by a decision**. The thing an on-demand base used to buy was
protection against single-region concentration, and random multi-region placement is
now itself that diversification: a Spot capacity event is scoped to one region, the
reconciler self-heals, and the client fails over across the directory. Set the
variable to `1` (and raise `monthly_budget_usd` with it) if you want the anchoring
back.

The public IPv4 address is the biggest line item per node, which is why the pool
is small and there is no NAT gateway (~$32/mo/region would blow the budget alone).
The **Budgets alert fires at 80% forecast ($16) and 100% actual ($20)**; `node_max`
and `on_demand_base_per_region` are the hard structural caps.

---

## Jurisdiction (§4)

The mechanism denies **by US state**, not by AWS region: a region is excluded iff
the state its AZs sit in appears in `state_denylist`. The map lives in
`region_state_map` and the denylist in `state_denylist` (variables.tf);
`jurisdiction.tf` enforces it at plan time, and the reconciler re-draws immediately
when a region drops out of the allowed set.

**The denylist is now empty — all four US regions are allowed.** It originally
denied Virginia (`us-east-1`), Ohio (`us-east-2`) and California (`us-west-1`) over
age-verification / device-level age-assurance laws, leaving Oregon as the only
candidate. That was placement **hygiene, not compliance**: those laws attach to
content services and to their users, never to server racks, so nothing was being
violated by hosting there. It was traded away deliberately — a pool that rotates
unpredictably across four regions is worth more than one pinned to a single state.
Re-denying is one line in `state_denylist`; the mechanism is unchanged.

## Testing

```bash
node --test                 # §3 directory contract (byte-exact + every reject case)
                            # + the random placement / rotation policy (seeded rng)
terraform validate          # config validity
terraform fmt -recursive -check
```

## Security posture

- Relay nodes hold **no Turso/DS/R2 credentials** — they authenticate devices
  offline (see `docs/relay-operations.md` §2). The SG opens only the relay UDP port
  (world) and the health TCP port (CIDR-scoped); egress is open because the relay
  binary's `POLLIS_RELAY_ALLOWLIST` is the real egress boundary.
- Least-privilege IAM: nodes read only the two identity params; the reconciler
  reads only its params, scales only `app=pollis-relay` ASGs, and writes only the
  one directory object.
- No SSH — shell access is SSM Session Manager only. IMDSv2 required.
- Signing/identity private material lives in SSM SecureStrings, never in TF state.
