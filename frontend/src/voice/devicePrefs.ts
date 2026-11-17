// Saved mic/speaker choices (#1989). The same rule as `pick` in pollis-core voice/devices.rs, which resolves a stale value at
// open time; this rewrites a pre-#1989 value in storage so it does not linger.

/** The saved value meaning "whatever the OS default is". */
const DEFAULT_AUDIO_DEVICE = "default";

/**
 * The value a saved choice should be stored as, given the devices listed now. A pre-#1989 value was a device name: macOS
 * and Windows saved the name a listed device still carries, so it becomes that device's id; Linux saved ALSA plugin names
 * (`pulse`, `pipewire`, `hw:…`) that no device carries, so it becomes the default. An id stays as it is even when its device
 * is not listed right now: Rust opens the default meanwhile and moves back when the device returns. An empty list
 * (enumeration failed) keeps everything: nothing to judge against.
 */
export function resolveSavedDevice(saved: string, devices: { id: string; name: string }[]): string {
  if (saved === DEFAULT_AUDIO_DEVICE || devices.length === 0 || isDeviceId(saved)) {
    return saved;
  }
  return devices.find((d) => d.name === saved)?.id ?? DEFAULT_AUDIO_DEVICE;
}

/** What the picker shows for a stored value: the device when listed, else the default it is falling back to. */
export function shownDevice(stored: string, devices: { id: string }[]): string {
  return devices.some((d) => d.id === stored) ? stored : DEFAULT_AUDIO_DEVICE;
}

/** cpal `DeviceId` strings are `<host>:<id>`; a pre-#1989 name never starts with a host. */
function isDeviceId(value: string): boolean {
  return /^(pulseaudio|alsa|coreaudio|wasapi|jack|pipewire):/.test(value);
}
