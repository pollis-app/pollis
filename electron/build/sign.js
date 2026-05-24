// Custom Windows code-signing hook for electron-builder, wired to Azure
// Trusted Signing (same provider the Tauri pipeline uses). Mirrors the
// signtool invocation in .github/workflows/desktop-release.yml so we
// don't fragment to two signing setups during the migration window.
//
// electron-builder calls this for every .exe / .dll inside the bundle
// that needs a signature. The config argument has the file path; we
// shell out to signtool with Azure's `/dlib` + `/dmdf` integration.
//
// Required env (set in the workflow):
//   SIGNTOOL_PATH        — absolute path to signtool.exe (from Windows SDK)
//   SIGNING_DLIB_PATH    — absolute path to Azure.CodeSigning.Dlib.dll
//   SIGN_METADATA_PATH   — absolute path to sign-metadata.json
//                          ({ Endpoint, CodeSigningAccountName, CertificateProfileName })
//   AZURE_TENANT_ID      — Service principal tenant (read by Azure dlib)
//   AZURE_CLIENT_ID      — Service principal client id
//   AZURE_CLIENT_SECRET  — Service principal client secret
//
// If SIGNTOOL_PATH or SIGNING_DLIB_PATH is unset, signing is skipped with
// a warning — lets local builds (no Azure creds) produce unsigned
// installers without exploding.

"use strict";

const { execFileSync } = require("child_process");

exports.default = function sign(configuration) {
  const signtool = process.env.SIGNTOOL_PATH;
  const dlib = process.env.SIGNING_DLIB_PATH;
  const metadata = process.env.SIGN_METADATA_PATH;

  if (!signtool || !dlib || !metadata) {
    console.warn(
      "[sign] Azure Trusted Signing env not set " +
        "(SIGNTOOL_PATH / SIGNING_DLIB_PATH / SIGN_METADATA_PATH) — " +
        "leaving '" +
        configuration.path +
        "' unsigned. Set the secrets in CI to enable.",
    );
    return;
  }

  const args = [
    "sign",
    "/v",
    "/fd",
    "SHA256",
    "/tr",
    "http://timestamp.acs.microsoft.com",
    "/td",
    "SHA256",
    "/dlib",
    dlib,
    "/dmdf",
    metadata,
    configuration.path,
  ];

  console.log("[sign] signing", configuration.path);
  execFileSync(signtool, args, { stdio: "inherit" });
};
