// When and whether to send this device's push token to the DS — decided with
// NO imports, so it runs under `node --test` (see `./routing.ts` for why).
//
// #1423: a DM could arrive with no notification because registration could
// silently stick. A failed `register_push_token` still marked the user as
// registered, so nothing retried it that session; registration only ran when a
// conversation was opened; and a rotated FCM token reached the DS only after a
// later chat open in a later session. So:
//
//   - a user counts as registered only once `register` has SUCCEEDED, and only
//     for the token that was sent;
//   - the app also registers, without prompting, on sign-in/unlock and every
//     return to the foreground (a no-op once registered);
//   - a token rotation re-fetches the token and re-registers if it changed.
//
// The native calls are injected, which is what keeps this file import-free.

export interface PushPermission {
  granted: boolean;
  canAskAgain: boolean;
}

export interface RegistrationDeps {
  getPermissions(): Promise<PushPermission>;
  requestPermissions(): Promise<{ granted: boolean }>;
  // Create the Android channel (no-op elsewhere).
  prepare(): Promise<void>;
  // Mint the Expo push token. Null when the build cannot (no EAS projectId).
  getToken(): Promise<string | null>;
  // POST the token to the DS. Throws on failure.
  register(userId: string, token: string): Promise<void>;
  warn(message: string, error?: unknown): void;
}

export interface EnsureOptions {
  // Whether the one-shot OS prompt may be shown. Only a contextual moment
  // (opening a conversation, the Preferences toggle) passes true; launch,
  // unlock, foreground and rotation never prompt.
  prompt?: boolean;
  // Re-fetch the token even if this user is already registered: a rotation.
  refresh?: boolean;
}

export interface PushRegistrar {
  ensure(userId: string, options?: EnsureOptions): Promise<string | null>;
  // The token last registered for `userId` this session, if any.
  registeredToken(userId: string): string | undefined;
}

export function createPushRegistrar(deps: RegistrationDeps): PushRegistrar {
  // userId -> the token the DS accepted for it this session.
  const registered = new Map<string, string>();
  // One flow per user at a time, so a foreground and a chat open landing
  // together POST once.
  const inFlight = new Map<string, Promise<string | null>>();

  async function run(userId: string, options: EnsureOptions): Promise<string | null> {
    if (registered.has(userId) && !options.refresh) {
      return null;
    }

    const current = await deps.getPermissions();
    let granted = current.granted;
    // Only fire the one-shot OS prompt where the caller allows it and the
    // system still can (the user hasn't already answered).
    if (!granted && options.prompt && current.canAskAgain) {
      granted = (await deps.requestPermissions()).granted;
    }
    if (!granted) {
      // Not handled: a later grant in Settings is picked up on the next
      // foreground.
      return null;
    }

    await deps.prepare();

    let token: string | null;
    try {
      token = await deps.getToken();
    } catch (e) {
      deps.warn("[push] getExpoPushTokenAsync failed:", e);
      return null;
    }
    if (!token) {
      return null;
    }
    if (registered.get(userId) === token) {
      // A rotation event that did not change the Expo token.
      return token;
    }

    try {
      await deps.register(userId, token);
    } catch (e) {
      // Not marked: the next foreground, unlock or chat open tries again.
      deps.warn("[push] register_push_token failed:", e);
      return null;
    }
    registered.set(userId, token);
    return token;
  }

  function ensure(userId: string, options: EnsureOptions = {}): Promise<string | null> {
    const pending = inFlight.get(userId);
    if (pending) {
      if (!options.refresh && !options.prompt) {
        return pending;
      }
      // A rotation, or a contextual prompt, during a quieter registration: run
      // again once that one is done, so the newer token ends up registered and
      // the prompt is not swallowed.
      return pending.then(() => ensure(userId, options));
    }
    const flow = run(userId, options).finally(() => {
      inFlight.delete(userId);
    });
    inFlight.set(userId, flow);
    return flow;
  }

  return {
    ensure,
    registeredToken(userId) {
      return registered.get(userId);
    },
  };
}
