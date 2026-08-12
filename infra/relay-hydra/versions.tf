terraform {
  # >= 1.10 because the R2 backend below uses `use_lockfile` (S3-native
  # conditional-write locking), which landed in 1.10. Older Terraform would
  # silently run WITHOUT locking against shared state.
  required_version = ">= 1.10.0"

  required_providers {
    # 6.x, not 5.x: the 5.x line stopped at 5.100 and its runtime validator
    # predates nodejs24.x, so it rejects the runtime this Lambda now needs.
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
    archive = {
      source  = "hashicorp/archive"
      version = "~> 2.4"
    }
  }

  # Remote state lives in CLOUDFLARE R2, not S3. Two reasons: we already run R2,
  # and keeping the state OUT of the AWS account it describes means losing or
  # compromising the AWS credentials does not also cost us the map of what
  # exists. It is the S3 backend pointed at R2's S3-compatible API, hence the
  # skip_* flags — R2 implements the object API but not STS, IMDS, or the
  # region/account plumbing the backend probes for by default.
  #
  # Locking is `use_lockfile` (S3-native conditional-write locking, Terraform
  # >= 1.10). NOT DynamoDB: that is AWS-only and would drag a second cloud back
  # in for one table. R2 supports the conditional writes this relies on.
  #
  # Credentials are an R2 API token scoped Object Read & Write on THIS BUCKET
  # ONLY (Doppler pollis/prd: R2_TFSTATE_ACCESS_KEY_ID / R2_TFSTATE_SECRET_KEY).
  # Deliberately not the app's R2 keys: those are scoped to the `pollis` bucket,
  # which is served publicly over cdn.pollis.com — and this state contains the
  # relay pool's PRIVATE QUIC identity key, so it must never live there.
  #
  # CAVEAT: R2 has no object versioning (PutBucketVersioning => NotImplemented),
  # so there is no bucket-level rollback of a bad state write. `init
  # -migrate-state` leaves a local backup, and scripts/snapshot-state.sh takes
  # dated copies — that is the recovery path, not S3-style version history.
  backend "s3" {
    bucket = "pollis-tfstate"
    key    = "relay-hydra/terraform.tfstate"
    region = "auto"

    # A DEDICATED profile, not the ambient AWS env/profile. The backend talks to
    # R2 and the provider talks to AWS; if both read AWS_ACCESS_KEY_ID they
    # collide and whichever set of keys is exported wins for BOTH — an R2 key
    # reaching the AWS provider fails STS with InvalidClientTokenId. Keeping the
    # backend on its own named profile means `AWS_PROFILE=pollis terraform plan`
    # is unambiguous: provider -> [pollis], state -> [r2-tfstate].
    profile = "r2-tfstate"

    endpoints = {
      s3 = "https://4bd9ab176c5febd5e7ac1b64b23dede5.r2.cloudflarestorage.com"
    }

    use_lockfile = true

    skip_credentials_validation = true
    skip_region_validation      = true
    skip_requesting_account_id  = true
    skip_metadata_api_check     = true
    skip_s3_checksum            = true
  }
}
