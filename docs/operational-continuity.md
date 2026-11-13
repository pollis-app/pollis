# Operational Continuity

**Status:** Live, and published at pollis.com/continuity (`website/continuity.html`, derived from this
document; edit here first). Pollis is operated by Pollis Inc.
**Audience:** the operator; anyone taking the service over; and a security researcher or auditor
reading the transparency log.
**Scope:** the accounts and keys the service depends on, what each one does, what happens in what
order if the service stops being operated, and how to read the transparency log while that is
happening. Opened under #877.

---

## 1. Asset register

The last column is what a *user* loses, not what is inconvenient.

| Asset | Where it lives | What it does | If it is lost |
|---|---|---|---|
| GitHub `pollis-app/pollis` | GitHub | Source, all CI, releases | No releases, no CI gates. The source already in the world stays (PolyForm Noncommercial) |
| Doppler (`pollis/prd`) | Doppler | Source of truth for service secrets; synced to GitHub Actions and the Wrangler Secrets Store | Every secret below must be reissued from its own provider |
| `TAURI_SIGNING_PRIVATE_KEY` (+ password) | GitHub secret | Signs the auto-updater manifests | **The worst single loss.** The matching public key is compiled into every shipped client (`src-tauri/tauri.conf.json` → `plugins.updater.pubkey`), so no future build can ever be accepted as an update by an already-installed app. Every user would have to reinstall by hand |
| `STH_SIGNING_KEY` | GitHub secret, backed up in Doppler | Signs all three transparency trees | The log cannot be republished until the offline root (#754) signs a key set naming a new signer, which installed clients accept without an update. A *leaked* key: see `docs/signing-key-compromise-runbook.md` |
| Apple: `APPLE_CERTIFICATE`, `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_PASSWORD` | GitHub secrets / Apple Developer | macOS signing + notarization; the App Store Connect record | No new macOS builds. Installed apps keep working; notarization of *existing* builds is already done |
| Azure Trusted Signing: `AZURE_CLIENT_ID/SECRET/TENANT_ID`, `AZURE_CERT_PROFILE` | GitHub secrets / Azure | Windows installer signing | No new Windows builds; SmartScreen warnings return for anything signed another way |
| Cloudflare account | Cloudflare | DNS for `pollis.com`, `api`, `cdn`, `verify`; Pages (website); Workers + containers (the DS, whose front-door Worker also serves the OTA updates at `api.pollis.com/updates/`); R2 (releases + the served transparency tree + mobile OTA updates under `ota/`) | **Everything user-facing stops.** This is the single largest concentration |
| Turso | Turso | Main DB and the commit-log DB | The DS cannot serve. Message *content* is unaffected — it is E2EE and the plaintext was never there |
| AWS account | AWS | The relay hydra (Terraform ASG, S3/CloudFront signed directory, reconciler Lambda, SSM state). CI reaches it by short-lived GitHub OIDC (`RELAY_IMAGE_OIDC_ROLE_ARN`), so there are no standing AWS keys in CI | The optional relay overlay stops rotating and eventually stops. Pollis still works; IP-metadata protection for those who enabled it does not |
| LiveKit + `VPS_SSH_KEY` | Own VPS | Voice/video/screenshare SFU | Calls stop. Text messaging is unaffected |
| Resend | Resend | Sends the email OTP | **Nobody can sign in or add a device.** Existing sessions keep working |
| `AUR_SSH_KEY` | GitHub secret / AUR | Publishes the Arch package | The AUR package goes stale. Already best-effort by design |
| `POLLIS_SIDELOAD_*` (Android sideload keystore + passwords) | Doppler `prd_prod` → GitHub secrets; keystore also at `~/.pollis/pollis-sideload.jks` on the release workstation | Signs the APK pollis.com offers outside Google Play (`mobile-apk-release.yml`); its certificate is pinned in `mobile/store/android-sideload-cert.sha256` and on pollis.com/android | Same shape as the updater key, for sideloaded Android installs: Android accepts an update only from the certificate already installed, so no future APK can update an existing sideload install. Every such user would have to uninstall (losing on-device history, unless they copy it back from another of their enrolled devices, #1735) and reinstall. Play installs are unaffected — Google holds their app-signing key |
| `OTA_CODE_SIGNING_KEY` (mobile OTA, #1250) | Doppler `prd_prod` → the `ota-signing` GitHub environment only; offline copy in 1Password | Signs every over-the-air JS update and rollback directive (`mobile-ota-release.yml`). Its certificate is compiled into every OTA-capable mobile build | Same shape as the updater key, for JS only: no new OTA update or rollback can be signed for builds that pin its certificate, so fixes reach those phones only through store/APK releases. Nothing breaks — phones keep running what they have, and store builds still ship. A *leaked* key is worse than a lost one: see `docs/signing-key-compromise-runbook.md` → "OTA code-signing key" |
| Offline transparency root key (#754) | Offline, never in CI or Doppler | Signs the key sets that name which keys may sign the log; its public half is pinned in every client (`PINNED_LOG_ROOT_KEYS`) | No new log signer can be authorised without a client release, so a lost `STH_SIGNING_KEY` becomes final again. **Leaked:** see `docs/signing-key-compromise-runbook.md` → "If the ROOT is compromised" |
| `POLLIS_OVERLAY_DIRECTORY_KEY` | GitHub secret | Signs the relay directory | Clients reject a directory nobody can re-sign; the overlay stops being usable |
| Expo/EAS, APNs key, FCM service account | EAS (held by EAS, not by this repo) | Mobile push | Push stops on every mobile install (TestFlight, Play testing tracks, sideload APK) until the credential is reissued with `eas credentials` |
| Play **upload key** (`pollis-upload.jks`) | Owner's workstation (`~/.pollis/`), passwords in `~/.gradle/gradle.properties` (`POLLIS_UPLOAD_*`) | Signs the Android App Bundle uploaded to Play | No new Play uploads until Google resets it. Play App Signing holds the key users' installs trust, so a reset (Play Console → App integrity → request upload key reset) changes nothing on devices. Losing the workstation means that reset. **Leaked:** the holder can sign bundles Play would accept from us, but still needs Play Console or the service account to upload one; request the reset at once — see `docs/signing-key-compromise-runbook.md` |
| Play publishing service account key | Doppler `prd_prod` `PLAY_SERVICE_ACCOUNT_JSON` (copy in 1Password) | `mobile/scripts/play-publish.py` uploads builds and the store listing | Uploads fall back to the Play Console by hand until a new key is issued in Google Cloud for `play-publisher@pollis.iam.gserviceaccount.com`. **Leaked:** the holder can upload builds to the testing tracks and edit the store listing (it needs a bundle signed with the upload key to ship code); delete the key in Google Cloud IAM, issue a new one, and update Doppler |
| App Store Connect API key (`AuthKey_*.p8`) | Owner's workstation (`~/.appstoreconnect/private_keys/`); `ASC_KEY_ID` / `ASC_ISSUER_ID` in Doppler `prd_prod` | Authenticates the iOS export (cloud signing) and the upload | No iOS builds or uploads until a new key is created in App Store Connect; nothing on devices depends on it. Losing the workstation means minting a new key (Users and Access → Integrations), which the Account Holder can do. **Leaked:** the holder can upload and manage builds, TestFlight testers and metadata; revoke it in App Store Connect at once and mint a new one |

**Two observations a reader should draw from that table.** First, the failures are mostly *availability*
and *distribution*, not confidentiality: no row above can read a message, because the servers never hold
plaintext or private message keys. (A *stolen* updater, OTA or sideload signing key is the exception: it can
sign a build that installed apps accept and that runs where messages are decrypted, which is why custody matters.) Second, the rows whose loss is genuinely unrecoverable — the
updater key, and for their builds the sideload and OTA keys — are **signing** keys whose public halves are pinned inside
software already installed on other people's machines. Pinning is what makes them strong and what makes
losing them final.

## 2. If the service stops being operated

Assume nobody intervenes and every bill keeps being paid.

| When | What happens | Visible to a user? |
|---|---|---|
| Immediately | Nothing. Messages deliver; the DS keeps serving; the site stays up | No |
| From its first failure | The daily transparency publish runs unattended until a run first fails (an expired token, a database or toolchain error); from then on new account keys and commits are not anchored | To anyone reading the publisher's run history (§3) |
| ~60 days | GitHub disables scheduled workflows on an inactive repository. Every cron in this repo stops | As above |
| Months | Apple/Azure signing credentials lapse on their own renewal cycles. No new signed builds can be made | On the next release |
| 12 months+ | The #720 device-staleness clock does its normal work: a device silent past `POLLIS_DS_WATERMARK_STALE_MONTHS` stops pinning envelope retention. This is designed behaviour | For a device offline that long |

Running services would keep running until a bill went unpaid or a certificate expired. Message content
stays end-to-end encrypted throughout.

## 3. Reading a stalled transparency log

The three transparency trees are republished by a daily cron (`transparency-publish.yml`). A Signed
Tree Head is `{tree_size, root_hash, timestamp, signature}`, and the publisher **reuses the frozen
timestamp from the published head for a given tree size unless the tree has grown**, so a head for a
size is byte-stable forever and cannot look like equivocation. From outside, three situations
therefore serve identical files:

1. the log is healthy and nothing has happened to publish;
2. the publisher has stopped;
3. someone is withholding publication of entries that exist.

**Detection.** What separates them is the publisher's own run history, which is public on a public
repository and needs no credential:

```
gh run list --workflow=transparency-publish.yml --limit 5 \
  --json conclusion,createdAt,url
```

**Escalation.**

1. **Owner, publisher red or absent > 48h:** re-dispatch `transparency-publish.yml`. If it fails on the
   signing key, follow `docs/signing-key-compromise-runbook.md`; if it fails on database access, treat it
   as a DS-side incident. Either way say so publicly: an unpublished log is a correctness problem.
2. **Owner, cannot be fixed within 7 days:** acknowledge it on `status.pollis.com`.
3. **Anyone, at any time:** the run history above is the record of what was published and when.

**What an observer should conclude.**

- A stalled publisher is evidence of nothing about honesty. The supported report is "the log is not
  currently being published".
- A served-but-stale head with a stopped publisher is not evidence of health either.
- What resolves it is keeping the heads you were served and comparing them later and with other people.
  Two parties holding signed heads that disagree have permanent, undeniable proof. That is what
  `pollis-verify` is released for.

## 4. Handover order

1. **GitHub org + repo.** Everything else is described in it.
2. **Doppler**, then the provider accounts behind each secret, since a Doppler value is only a copy.
3. **Cloudflare**, which is DNS, the website, the DS and the CDN in one account.
4. **Turso**, both databases.
5. **The signing keys**, last and most carefully: `TAURI_SIGNING_PRIVATE_KEY`, `STH_SIGNING_KEY` and the
   offline transparency root. Custody, rotation and the runbook: `docs/sth-signing-key-custody.md`,
   `docs/signing-key-compromise-runbook.md`. Clients pin an offline root as well as the log signer
   (#754, #806), so a root-signed key set can name a new signer without a client release.

## 5. Standing commitments

- **This register is part of the repository**, so a new credential added without a row here is visible
  in review as an asymmetry between `.github/workflows/` and §1.
- **An unpublished or unverifiable log is a correctness incident**, acknowledged on `status.pollis.com`.

## Related

`docs/deployments.md` (what ships where, and the post-merge checklist) ·
`docs/sth-signing-key-custody.md` ·
`docs/signing-key-compromise-runbook.md` ·
`docs/security-whitepaper.md` ·
`status.pollis.com` (service status)
