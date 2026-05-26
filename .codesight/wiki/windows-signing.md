# Windows Code Signing — Azure Trusted Signing

Pollis signs Windows `.exe` artifacts (inner binary + NSIS installer) using **Azure Trusted Signing**. Signing runs on the `windows-latest` GitHub Actions runner via `electron-builder`'s `signtoolOptions.sign` hook (`electron/build/sign.js`), which wraps `signtool.exe` with Azure's `/dlib` + `/dmdf` integration. The private key lives in Azure's HSM — no PFX, no hardware token.

> Note: in earlier names of the service this was called "Azure Artifact Signing". The Azure resources below still appear under that namespace in the `az` CLI surface (`az artifact-signing …`) even though Microsoft markets the service as Trusted Signing today.

This article documents the Azure-side state so a future migration (e.g. individual → organization tenant) can be reproduced from a clean slate.

## Azure resources (current state)

| Resource | Name | Value |
|---|---|---|
| Subscription | Azure subscription 1 | `fdf7ad4c-2f79-4935-8259-3be73713664b` |
| Tenant | Default Directory | `0054f492-25f1-4dfe-9ff6-41d68e158435` |
| Resource group | `Pollis` | region `eastus` |
| Artifact Signing account | `pollis` | endpoint `https://eus.codesigning.azure.net` |
| Identity validation | individual — Daniel Kral | portal-only; ID surfaced in portal only |
| Certificate profile | `pollis-public` | type `PublicTrust` |
| App registration (CI) | `pollis-signing-ci` | appId `2427b69e-6fa5-4c4f-a830-7ca86897c7c1` |

Certificates under Artifact Signing have a **3-day rolling validity**, so timestamping every signature (`/tr http://timestamp.acs.microsoft.com`) is mandatory.

## RBAC roles

Two distinct roles, assigned to two distinct principals at two distinct scopes:

| Role | Assignee | Principal type | Scope | Purpose |
|---|---|---|---|---|
| **Artifact Signing Identity Verifier** | the human admin | User | signing account | Submit identity validation requests in the portal |
| **Artifact Signing Certificate Profile Signer** | `pollis-signing-ci` | ServicePrincipal | certificate profile | Sign binaries from CI |

Scope the **Signer** role to the certificate profile, not the account — keeps least privilege and prevents one SP from signing with a future profile it shouldn't have access to.

## GitHub Actions secrets

All stored in Doppler and synced to the GitHub repo.

| Secret | Value shape | Source |
|---|---|---|
| `AZURE_TENANT_ID` | GUID | `az account show` |
| `AZURE_CLIENT_ID` | GUID (appId) | `az ad app create` |
| `AZURE_CLIENT_SECRET` | opaque | `az ad app credential reset` |
| `AZURE_SIGNING_ACCOUNT` | account name | Signing account resource |
| `AZURE_CERT_PROFILE` | profile name | Certificate profile resource |
| `AZURE_SIGNING_ENDPOINT` | `https://<region>.codesigning.azure.net` | Region table in MS docs — **no trailing slash** |

The dlib authenticates via `DefaultAzureCredential`, which picks up `AZURE_TENANT_ID/CLIENT_ID/CLIENT_SECRET` from the environment automatically.

## Provisioning from scratch (new tenant / re-do)

Steps 2 and 4 are **portal-only** — no CLI path exists. Everything else is scripted.

1. **Register provider** (one-time per subscription):
   ```bash
   az provider register --namespace Microsoft.CodeSigning
   az extension add --name artifact-signing
   ```

2. **Create signing account** (portal: search "Artifact Signing Accounts" → Create). Pick region — the endpoint URL is region-specific (see [MS region table](https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart#azure-regions-that-support-artifact-signing)).

3. **Grant Identity Verifier role to yourself** so the portal lets you submit validation:
   ```bash
   USER_OBJECT_ID=$(az ad signed-in-user show --query id -o tsv)
   SCOPE="/subscriptions/<sub>/resourceGroups/<rg>/providers/Microsoft.CodeSigning/codeSigningAccounts/<account>"
   az role assignment create \
     --role "Artifact Signing Identity Verifier" \
     --assignee-object-id "$USER_OBJECT_ID" \
     --assignee-principal-type User \
     --scope "$SCOPE"
   ```

4. **Submit identity validation** (portal → signing account → Identity validations → + New identity → Public).
   - Individual: government ID + selfie via AU10TIX (USA / Canada only).
   - Organization: business docs, D-U-N-S optional. Available in US, CA, EU, UK. Processing 1–20 business days.
   - After approval, copy the **Identity validation Id** GUID from the detail pane.

5. **Create certificate profile**:
   ```bash
   az artifact-signing certificate-profile create \
     -g <rg> --account-name <account> \
     -n pollis-public \
     --profile-type PublicTrust \
     --identity-validation-id <guid-from-step-4>
   ```

6. **Create CI service principal**:
   ```bash
   APP_ID=$(az ad app create --display-name pollis-signing-ci --query appId -o tsv)
   az ad sp create --id "$APP_ID"
   az ad app credential reset --id "$APP_ID" --display-name github-actions --years 2
   ```

7. **Grant Signer role to the service principal** scoped to the profile:
   ```bash
   SP_OBJECT_ID=$(az ad sp show --id "$APP_ID" --query id -o tsv)
   az role assignment create \
     --role "Artifact Signing Certificate Profile Signer" \
     --assignee-object-id "$SP_OBJECT_ID" \
     --assignee-principal-type ServicePrincipal \
     --scope "/subscriptions/<sub>/resourceGroups/<rg>/providers/Microsoft.CodeSigning/codeSigningAccounts/<account>/certificateProfiles/pollis-public"
   ```

8. **Update Doppler** with the six secrets from the table above. The CI workflow (`.github/workflows/electron-release.yml`) picks them up automatically; `electron-builder`'s sign hook reads them at signing time and passes them into the Azure dlib.

## Migrating to an organization tenant

If we switch from the current individual identity to a business-entity validation:

1. Run steps 1–8 above in the new tenant/subscription. Different account name is fine — just update the Doppler secret.
2. The existing signer certificates under `pollis-public` become stranded; old signed installers remain valid until their timestamp-anchored signature expires (timestamping anchors validity to signing time, not cert validity).
3. **Do not delete** the old signing account immediately — keep it long enough that the final release signed under the individual identity is no longer the "latest" most users are running.
4. Publisher name on the signed installer will change (from `O=Daniel Kral` → `O=<LegalEntity>`). SmartScreen reputation resets per-publisher — a short warning period is expected after the switch until the new publisher accrues reputation. Microsoft-issued certs build reputation faster than 3rd-party OV certs, typically hours not weeks.

## Related links

- [Artifact Signing quickstart](https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart)
- [SignTool integration](https://learn.microsoft.com/en-us/azure/artifact-signing/how-to-signing-integrations)
- [Resources and roles reference](https://learn.microsoft.com/en-us/azure/artifact-signing/concept-resources-roles)
- Original issue: [#118](https://github.com/actuallydan/pollis/issues/118)
