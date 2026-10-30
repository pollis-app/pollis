// When to send this device's push token to the DS (#1423): registered only once `register` SUCCEEDED, for that token. Import-free.

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
  register(userId: string, token: string): Promise<void>;
  warn(message: string, error?: unknown): void;
}

export interface EnsureOptions {
  // Only a contextual moment passes true.
  prompt?: boolean;
  refresh?: boolean;
}

export interface PushRegistrar {
  ensure(userId: string, options?: EnsureOptions): Promise<string | null>;
  registeredToken(userId: string): string | undefined;
}

export function createPushRegistrar(deps: RegistrationDeps): PushRegistrar {
  const registered = new Map<string, string>();
  // One flow per user at a time.
  const inFlight = new Map<string, Promise<string | null>>();

  async function run(userId: string, options: EnsureOptions): Promise<string | null> {
    if (registered.has(userId) && !options.refresh) {
      return null;
    }

    const current = await deps.getPermissions();
    let granted = current.granted;
    // Only where the caller allows it and the system still can.
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
      // Not marked, and any earlier success is forgotten too: the DS may still hold an older
      // token (a rotation whose register failed), so the next foreground retries (#1610).
      registered.delete(userId);
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
      // Run again after the in-flight one, so the newer token or the prompt is not swallowed.
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

/**
 * What may ask for a registration, and when (#1620). Registering needs a DS signature, which the
 * device can make only past the PIN, so nothing asks while the session is locked or before sign-in:
 * on a cold start the PIN screen sat under a registration that failed, and before #1610 under a
 * loop of them. Each attempt answers one event, and a failure is retried only by the next one.
 *
 * - `setUser`: the signed-in, unlocked user, or null. Becoming non-null (sign-in, unlock) registers
 *   once; the same user again (a re-render) does nothing.
 * - `foreground`: a return to the foreground, which retries a failed registration.
 * - `rotation`: the OS rotated the token. While locked it is remembered and refreshed on the unlock.
 */
export interface RegistrationTriggers {
  setUser(userId: string | null): void;
  foreground(): void;
  rotation(): void;
}

export function createRegistrationTriggers(
  ensure: (userId: string, options: EnsureOptions) => unknown,
): RegistrationTriggers {
  let user: string | null = null;
  let rotatedWhileAway = false;
  return {
    setUser(next) {
      if (next === user) {
        return;
      }
      user = next;
      if (next === null) {
        return;
      }
      const refresh = rotatedWhileAway;
      rotatedWhileAway = false;
      void ensure(next, { prompt: false, refresh });
    },
    foreground() {
      if (user !== null) {
        void ensure(user, { prompt: false });
      }
    },
    rotation() {
      if (user === null) {
        rotatedWhileAway = true;
        return;
      }
      void ensure(user, { prompt: false, refresh: true });
    },
  };
}

/**
 * Tells a real token rotation from the echo of our own request (#1610). expo-notifications fires
 * its push-token event on EVERY registration, including the one `getToken` itself triggers on
 * iOS, so a listener that re-registered on each event looped for as long as the app ran: event,
 * getToken, event... (on the simulator, where the DS refuses the token, a signed DS call about
 * every 170 ms).
 *
 * A token is a rotation unless it is the one our latest request obtained (its echo) or the last
 * one the DS ACCEPTED, which only a successful register records. A rotation whose register fails
 * is therefore not retried from its own echo (that was the loop) but by the next foreground: the
 * registrar forgets the user's registration on any failed register.
 */
export interface TokenEchoFilter {
  /** The device token our own request just obtained. */
  noteOwn(deviceToken: string): void;
  /** The DS accepted the token of our latest request. */
  confirmOwn(): void;
  /** Whether an event's token is new. Records nothing. */
  isRotation(deviceToken: string): boolean;
}

export function createTokenEchoFilter(): TokenEchoFilter {
  let own: string | null = null;
  let accepted: string | null = null;
  return {
    noteOwn(deviceToken) {
      own = deviceToken;
    },
    confirmOwn() {
      if (own !== null) {
        accepted = own;
      }
    },
    isRotation(deviceToken) {
      return deviceToken !== own && deviceToken !== accepted;
    },
  };
}
