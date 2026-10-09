/*
 * A throwaway OTA code-signing identity for the tests — generated fresh in
 * memory on every run, never written to disk, never committed, and never the
 * production certificate (that is mobile/store/ota-code-signing.pem, whose key
 * only the owner and the protected `ota-signing` environment hold).
 *
 * The certificate comes from @expo/code-signing-certificates — the library
 * `expo-updates codesigning:generate` (and so scripts/generate-ota-signing-key.sh)
 * uses — so it carries exactly the extensions the app's verifier demands.
 */

import { generateKeyPairSync } from "node:crypto";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const certs = require("@expo/code-signing-certificates");

export interface TestSigner {
  privateKeyPem: string;
  certPem: string;
}

export function makeTestSigner(commonName = "Pollis OTA TEST ONLY"): TestSigner {
  const { privateKey, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
  const privateKeyPem = privateKey.export({ type: "pkcs1", format: "pem" }).toString();
  const publicKeyPem = publicKey.export({ type: "spki", format: "pem" }).toString();
  const keyPair = certs.convertKeyPairPEMToKeyPair({ privateKeyPEM: privateKeyPem, publicKeyPEM: publicKeyPem });
  const now = new Date();
  const cert = certs.generateSelfSignedCodeSigningCertificate({
    keyPair,
    validityNotBefore: new Date(now.getTime() - 60_000),
    validityNotAfter: new Date(now.getTime() + 24 * 3600_000),
    commonName,
  });
  certs.validateSelfSignedCertificate(cert, keyPair);
  return { privateKeyPem, certPem: certs.convertCertificateToCertificatePEM(cert) };
}

// Expo's own signer, to prove our verifier accepts what Expo's tooling signs.
export function expoSign(signer: TestSigner, body: string): string {
  const key = certs.convertPrivateKeyPEMToPrivateKey(signer.privateKeyPem);
  const cert = certs.convertCertificatePEMToCertificate(signer.certPem);
  return certs.signBufferRSASHA256AndVerify(key, cert, Buffer.from(body, "utf8"));
}
