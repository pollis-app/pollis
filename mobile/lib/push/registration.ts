// When and whether to send this device's push token to the DS (#1423). Native
// calls are injected so the file stays import-free for `node --test`.
//
// A user counts as registered only once `register` SUCCEEDED, and only for the
// token sent; the app re-runs this without prompting on sign-in/unlock and
// every foreground, and on token rotation.

export interface PushPermission {
  granted: boolean;
  canAskAgain: boolean;
}

export interface RegistrationDeps {
  getPermissions(): Promise<PushPermission>;
  requestPermissions(): Promise<{ granted: boolean }>;
  prepare(): Promise<void>;
  // Null when the build cannot mint one (no EAS projectId).
  getToken(): Promise<string | null>;
  // Throws on failure.
  register(userId: string, token: string): Promise<void>;
  warn(message: string, error?: unknown): void;
}

export interface EnsureOptions {
  // Whether the one-shot OS prompt may be shown: only at a contextual moment
  // (opening a conversation, the Preferences toggle), never at launch, unlock,
  // foreground or rotation.
  prompt?: boolean;
  // Re-fetch the token even if already registered: a rotation.
  refresh?: boolean;
}

export interface PushRegistrar {
  ensure(userId: string, options?: EnsureOptions): Promise<string | null>;
  registeredToken(userId: string): string | undefined;
}

export function createPushRegistrar(deps: RegistrationDeps): PushRegistrar {
  const registered = new Map<string, string>();
  // One flow per user at a time, so simultaneous triggers POST once.
  const inFlight = new Map<string, Promise<string | null>>();

  async function run(userId: string, options: EnsureOptions): Promise<string | null> {
    if (registered.has(userId) && !options.refresh) {
      return null;
    }

    const current = await deps.getPermissions();
    let granted = current.granted;
    // Only where the caller allows it and the user hasn't already answered.
    if (!granted && options.prompt && current.canAskAgain) {
      granted = (await deps.requestPermissions()).granted;
    }
    if (!granted) {
      // A later grant in Settings is picked up on the next foreground.
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
      // A rotation or a contextual prompt during a quieter registration: run
      // again afterwards, so the newer token lands and the prompt isn't swallowed.
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
