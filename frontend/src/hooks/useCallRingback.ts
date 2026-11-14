import { useEffect } from "react";
import { autorun } from "mobx";
import { invoke } from "../bridge";
import { appStore } from "../stores/appStore";
import { logIgnored } from "../utils/log";
import { Ringback, ringbackWanted } from "../voice/ringback";

// The loop runs in Rust (`start_ringback`), on the same output and handle as every other sound effect.
const ringback = new Ringback({
  start: () => {
    invoke("start_ringback").catch(logIgnored);
  },
  stop: () => {
    invoke("stop_ringback").catch(logIgnored);
  },
});

/** #1901: the caller hears a ringback while their DM call rings. `autorun`, so the shell does not re-render per participant. */
export function useCallRingback(soundEnabled: boolean): void {
  useEffect(() => {
    return autorun(() => {
      ringback.sync(ringbackWanted(appStore, soundEnabled));
    });
  }, [soundEnabled]);

  // Leaving the shell (logout, lock) never leaves a tone looping.
  useEffect(() => {
    return () => {
      ringback.sync(false);
    };
  }, []);
}
