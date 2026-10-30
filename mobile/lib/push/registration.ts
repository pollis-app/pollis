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
      // Not marked: retried next time.
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
 * Tells a real token rotation from the echo of our own request (#1610). expo-notifications fires
 * its push-token event on EVERY registration, including the one `getToken` itself triggers on
 * iOS, so a listener that re-registered on each event looped for as long as the app ran: event,
 * getToken, event... (on the simulator, where the DS refuses the token, a signed DS call about
 * every 170 ms). Only a device token not seen before is a rotation.
 */
export interface TokenEchoFilter {
  /** The device token our own request just obtained. */
  noteOwn(deviceToken: string): void;
  /** Whether an event's token is new, recording it either way. */
  isRotation(deviceToken: string): boolean;
}

export function createTokenEchoFilter(): TokenEchoFilter {
  let known: string | null = null;
  return {
    noteOwn(deviceToken) {
      known = deviceToken;
    },
    isRotation(deviceToken) {
      if (deviceToken === known) {
        return false;
      }
      known = deviceToken;
      return true;
    },
  };
}
