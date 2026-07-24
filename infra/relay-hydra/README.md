# The hydra — automated AWS relay-pool orchestrator (#616)

IaC + reconciler + signed directory for the Pollis closed-overlay relay pool.
This is the **AWS-hosting half**; the relay binary, its image, the client, and the
client's directory-fetch are the monorepo/#455 workstream. The only coupling is
the **§3 signed-directory contract** (`lib/directory-verify.mjs`), proven byte-
exact by `test/directory-contract.test.mjs`.

```
Terraform ──> per-region VPC + locked SG + mixed-instances ASG (t4g.nano, Spot floor on-demand)
          ──> S3 (private) + CloudFront (OAC)  ── serves the signed directory
          ──> reconciler Lambda (EventBridge every 2 min) ── scales ASG, health-checks /version,
                                                              signs + publishes the directory
          ──> Budgets $20 alert + CloudWatch alarms
SSM (free, SecureString) ── signing private key · pool QUIC identity · desired-state
```

There is **no load balancer**: clients fetch the signed directory and do their own
health/failover. Each relay is just a node with a public UDP port.

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
The reconciler reads desired-state from SSM and converges within one cycle (~2 min).
Terraform seeds it once, then leaves it alone.
```bash
aws ssm put-parameter --region us-west-2 --overwrite \
  --name /pollis/relay-hydra/desired-state --type String \
  --value '{"us-west-2": 3}'
# force an immediate reconcile instead of waiting for the schedule:
aws lambda invoke --function-name "$(terraform output -raw reconciler_function_name)" /dev/stdout
```
Counts are clamped to `[node_floor, node_max]`. To raise the ceiling, bump
`node_max` in tfvars and re-apply (mind the $20 cap — see cost below).

### Add / remove a region
1. Confirm the region's US state is **clean** (no age-verification / device-OS
   age-registration law) and present in `region_state_map` (variables.tf).
2. Add an **aliased provider** for the region in `providers.tf`, and a second
   `module "relay_region_<r>"` block passing that provider (the module is fully
   region-parameterized — that's the only code edit).
3. Add it to `region_node_counts` and apply.

The jurisdiction guard (`jurisdiction.tf`) **fails the plan** if a requested region
maps to a denied or unmapped state — this is the enforced default-deny.

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

### Tear it all down
```bash
terraform destroy
# The signing/identity SSM SecureStrings are NOT Terraform-managed (their plaintext
# must never touch TF state) — delete them explicitly:
aws ssm delete-parameters --region us-west-2 --names \
  /pollis/relay-hydra/signing-key /pollis/relay-hydra/identity-key \
  /pollis/relay-hydra/identity-cert /pollis/relay-hydra/desired-state
```

---

## Cost (§0 hard target: < $20/month)

Per-node, us-west-2, all-in:

| Item | On-demand node | Spot node |
| --- | --- | --- |
| `t4g.nano` compute (730 h) | ~$3.07 | ~$0.95 |
| Public IPv4 (the dominant cost) | ~$3.65 | ~$3.65 |
| 8 GiB gp3 EBS | ~$0.64 | ~$0.64 |
| **Per node** | **~$7.36** | **~$5.24** |

Default config = `node_floor = 2` (on-demand, guaranteed) + up to 1 Spot at
`node_max = 3`:

- **Steady state (floor, 2 nodes): ~$14.7/mo** — comfortably under.
- **Full burst (3 nodes = 2 on-demand + 1 Spot): ~$19.9/mo** — at the cap.
- Lambda + S3 + CloudFront + SSM (standard tier, free) + EventBridge + a handful of
  CloudWatch alarms ($0.10 each): **< $1/mo**.

The public IPv4 address is the biggest line item per node, which is why the pool
is small and there is no NAT gateway (~$32/mo/region would blow the budget alone).
The **Budgets alert fires at 80% forecast ($16) and 100% actual ($20)**; `node_max`
and the on-demand floor are the hard structural caps.

---

## Jurisdiction (§4)

Placement is denied **by US state**, not by AWS region: a region is excluded iff
the state its AZs sit in has an age-verification or device/OS age-registration law.
As of mid-2026 that denies Virginia (`us-east-1`), Ohio (`us-east-2`), California
(`us-west-1`) — leaving **Oregon (`us-west-2`)** as the only clean US region. The
map lives in `region_state_map` and the denylist in `state_denylist` (variables.tf);
`jurisdiction.tf` enforces it at plan time. **Re-check the state-law landscape
before adding any region.**

## Testing

```bash
node --test                 # the §3 directory contract, byte-exact + every reject case
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
