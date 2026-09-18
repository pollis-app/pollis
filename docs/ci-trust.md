# CI trust boundary — what the repository cannot enforce about itself

Everything in `.github/` is only as trustworthy as the account and settings around it.
A workflow file can pin its actions, drop its permissions and refuse a bad input — and
`scripts/check-action-pins.py`, `scripts/check-workflows.py` and
`scripts/check-build-recipe.py` gate exactly that on every PR. None of it helps if
someone can push straight to `main`, move a `v*` tag, or dispatch `desktop-release.yml`
by hand, because at that point they are the release.

This file is the list of controls that live in **GitHub settings**, not in this
repository: who may change what, and what breaks if the control is off. It is written
to be actionable by a repository owner in one sitting, and to be re-checkable later —
each item says what "configured" looks like, so a drifted setting is visible rather
than assumed.

## What the repo already enforces (context, not a to-do)

- Every `uses:` in `.github/workflows/**` and `.github/actions/**` is pinned to a full
  commit SHA with its version as a trailing comment; `scripts/check-action-pins.py`
  fails `scripts-check.yml` on anything mutable, and `.github/dependabot.yml` bumps the
  pins weekly. A retagged upstream action can no longer reach our runners.
- Release build jobs export only the public `option_env!` recipe to `$GITHUB_ENV`; the
  R2 write key and the LiveKit secret are step-scoped `env:` on the exact steps that
  use them (`scripts/check-build-recipe.py`).
- Workflow `permissions:` default to `contents: read`, with `contents: write` /
  `id-token: write` declared per job.
- `desktop-release.yml` refuses a `workflow_dispatch` run outright — its first job
  fails unless the ref is a `v*` tag.

## Settings-only controls

### 1. `main` ruleset: reviewed, checked, no direct pushes

*Settings → Rules → Rulesets → the `main` ruleset.*

- **Require a pull request before merging, with ≥ 1 approving review.** The ruleset
  currently requires **0** approvals (`docs/deployments.md` records this), so a single
  compromised or careless account can merge anything it opens — including a change to
  the workflows that then runs with release secrets on the next tag. Raise it to 1.
- **Dismiss stale approvals on new commits**, so an approval cannot be recycled onto a
  different diff.
- **Required status checks**, at minimum: `gate` (`mls-tests.yml`), `windows-gate`
  (`windows-link.yml`), `shell-scripts` (`scripts-check.yml` — the action-pin,
  workflow-parse and build-recipe gates all live in that job), `frontend-check`,
  `e2e-browser`, `supply-chain`. A check that is not marked required is advisory: a red
  one merges.
- **Block force pushes** and **restrict deletions** on `main`.
- **Do not** grant bypass to any actor beyond what is strictly needed. The
  `rebuild-ledger` bot needs PR *creation* (the "Allow GitHub Actions to create and
  approve pull requests" toggle), never a review bypass.

Note the interaction: raising the approval requirement to 1 means the
`bot/rebuild-ledger` PR needs a human approval as well as a human merge. That is the
intended cost — see `docs/deployments.md`.

### 2. Tag protection for `v*` — the tag *is* the release trigger

*Settings → Rules → Rulesets → new ruleset, target **Tags**, pattern `v*` (and
`pollis-verify-v*`).*

`desktop-release.yml` and `cli-release.yml` fire on a `v*` tag push, and
`verifier-release.yml` on `pollis-verify-v*`. Anyone who can create or **move** such a
tag can publish a signed release from arbitrary tree contents, and a moved tag also
rewrites what the transparency log's leaf claims to describe.

- **Restrict creation** to the release maintainers.
- **Restrict updates and deletions** — a release tag must be immutable. This is the
  single highest-value item on this page.

### 3. Release secrets belong in Environments with required reviewers

*Settings → Environments.*

Today the deploy workflows use environments (`delivery-dev`, `delivery-prod`,
`db-dev`, `livekit-*`, `website-prod`) but the **release** secrets are repository-level:
`APPLE_*` (notarization + Developer ID), `AZURE_*` (Trusted Signing),
`TAURI_SIGNING_PRIVATE_KEY*` (the updater key), `R2_*` (writes `cdn.pollis.com`:
`install.sh`, `latest.json`, every installer), `STH_SIGNING_KEY` and
`LOG_DB_ADMIN_TOKEN` (the transparency log's append credential), `AUR_SSH_KEY`,
`TURSO_*`. A repository-level secret is readable by **every** workflow the repo runs,
including one added in a PR that merges without review.

- Create a `release` environment (and a `transparency` one for `STH_SIGNING_KEY` /
  `LOG_DB_ADMIN_TOKEN`), move those secrets into it, and add **required reviewers**.
- Scope each environment to the protected `v*` tags via its deployment-branch/tag rule,
  so a branch build cannot select it.
- Then add `environment: release` to the jobs that need them. That part *is* a repo
  change, but it is inert until the environment exists — so it is listed here, to be
  done in the same sitting.
- Rotation runbooks for the signing material: `docs/signing-key-compromise-runbook.md`,
  `docs/sth-signing-key-custody.md`.

### 4. Restrict `workflow_dispatch` on the release and deploy workflows

Dispatch permission follows repository **write** access — there is no per-workflow
setting — so the control is: keep the write-access list small, and put the credentials
behind an Environment with reviewers (item 3), which is what actually gates a dispatched
run. Workflows where a hand-fired run publishes or mutates something real:
`cli-release.yml`, `verifier-release.yml`, `attest-release.yml`,
`transparency-publish.yml`, `aur-republish.yml`, `website-deploy.yml`,
`delivery-deploy-{dev,prod}.yml`, `db-migrate-dev.yml`, `livekit-deploy.yml`,
`relay-image.yml`. (`desktop-release.yml` already self-refuses a dispatch.)

### 5. Fork-PR and runner settings

*Settings → Actions → General.*

- **Fork pull request workflows from outside collaborators: require approval for all
  outside collaborators.** Without it, a first-time contributor's PR runs workflow code
  on our runners.
- **Workflow permissions: read repository contents by default** (the workflows already
  declare their own), and leave **"Allow GitHub Actions to create and approve pull
  requests" on** — `rebuild-ledger.yml` depends on it, see `docs/deployments.md`.
- Restrict which actions may run to **"Allow enterprise, and select non-enterprise,
  actions and reusable workflows"**, listing the owners the workflows actually use
  (`grep -rhoE 'uses: [^/]+/' .github/workflows` today gives `actions/*`, `docker/*`, `dtolnay/*`,
  `Swatinem/*`, `pnpm/*`, `sigstore/*`, `softprops/*`, `anchore/*`, `aws-actions/*`,
  `dorny/*`, `dopplerhq/*`, `taiki-e/*`, `tauri-apps/*`). Belt and
  braces with the SHA pins: this one also stops a *new* unpinned action being added.
- If self-hosted runners are ever attached, they must not be reachable from public-fork
  PRs. None are attached today.

### 6. Account and organization hygiene

- **2FA required** for every account with write access; hardware keys for anyone who
  can move a `v*` tag.
- Audit **deploy keys, PATs and GitHub Apps** with write scope — each is an alternative
  path to a tag push that no ruleset bypass list mentions.
- Turn on **secret scanning with push protection**, so a credential committed by
  accident is refused rather than rotated afterwards.

## Re-check cadence

Walk this page whenever the release surface changes — a new signing credential, a new
publishing workflow, a change to who holds write access — and at minimum alongside the
post-merge release checklist in `docs/deployments.md` when that checklist touches a
release pipeline.
