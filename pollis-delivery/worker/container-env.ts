// The container's environment must follow the deployed config (#1611).
//
// A container gets its environment when its Durable Object starts it. A deploy that rolls the
// image starts the replacement instance itself, and that instance keeps the environment the
// PREVIOUS start passed — so a var or secret added (or changed) in the deploy never reaches the
// running DS until the container next stops on its own. #1611's POLLIS_CLIENT_POLICY_URL sat
// missing on api-dev for exactly this reason while every older var was present.
//
// So each start records a fingerprint of the environment it passed, and the first request a
// freshly deployed Durable Object handles compares it with the environment it would pass now. A
// mismatch (or no record) restarts the container once, with the current environment. Only a
// SHA-256 of the values is stored: secrets never land in Durable Object storage.

/** Durable Object storage key of the last start's fingerprint. */
export const ENV_FINGERPRINT_KEY = "container-env-fingerprint";

/** Order-independent SHA-256 of an environment, hex. */
export async function envFingerprint(env: Record<string, string>): Promise<string> {
  const canonical = JSON.stringify(Object.keys(env).sort().map((k) => [k, env[k]]));
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(canonical));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** Restart a running container whose recorded start environment is not the current one. */
export function needsRestart(running: boolean, recorded: string | undefined, current: string): boolean {
  return running && recorded !== current;
}
