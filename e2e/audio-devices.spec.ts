// Microphone / Speaker pickers (#1989): the list is the audio server's devices by description (cpal device ids), the OS
// default is always the first entry, a choice saves and switches by id, a value an older release saved resolves to what
// it names now, and a device change reported by Rust refetches the list.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const YETI = "pulseaudio:alsa_input.usb-Blue_Microphones_Yeti-00.analog-stereo";
const AIRPODS = "pulseaudio:bluez_output.AA_BB_CC_DD_EE_FF.1";

type TestWindow = Window & {
  __tauriInvokeCounts: Record<string, number>;
  __tauriLastArgs: Record<string, Record<string, unknown>>;
  __pollisVoice: { injectEvent: (event: unknown) => void };
};

async function openVoiceSettings(page: Page, saved?: Record<string, string>) {
  await page.addInitScript(
    ({ preload, saved }) => {
      (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
      if (saved) {
        localStorage.setItem("pollis:voice-devices", JSON.stringify(saved));
      }
    },
    { preload: { session: ME, profile: { id: ME.id, username: ME.username } }, saved },
  );
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("sidebar-row-voice-settings").click();
  await expect(page.getByTestId("voice-settings-page")).toBeVisible();
}

function savedDevices(page: Page): Promise<Record<string, string>> {
  return page.evaluate(() => JSON.parse(localStorage.getItem("pollis:voice-devices") || "{}"));
}

function options(page: Page, testId: string) {
  return page.getByTestId(testId).locator("option").evaluateAll((els) =>
    els.map((el) => [(el as HTMLOptionElement).value, el.textContent]),
  );
}

function invokeCount(page: Page, cmd: string): Promise<number> {
  return page.evaluate((c) => (window as unknown as TestWindow).__tauriInvokeCounts[c] ?? 0, cmd);
}

test.describe("audio device pickers", () => {
  test("list devices by description with the default first", async ({ page }) => {
    await openVoiceSettings(page);
    expect(await options(page, "voice-input-select")).toEqual([
      ["default", "Default microphone"],
      ["pulseaudio:alsa_input.pci-0000_00_1f.3.analog-stereo", "Built-in Audio Analog Stereo"],
      [YETI, "Yeti Stereo Microphone"],
    ]);
    expect(await options(page, "voice-output-select")).toEqual([
      ["default", "Default speaker"],
      ["pulseaudio:alsa_output.pci-0000_00_1f.3.analog-stereo", "Built-in Audio Analog Stereo"],
      [AIRPODS, "AirPods Pro"],
    ]);
    await expect(page.getByTestId("voice-input-select")).toHaveValue("default");
  });

  test("a choice saves and switches by device id", async ({ page }) => {
    await openVoiceSettings(page);
    await page.getByTestId("voice-input-select").selectOption({ label: "Yeti Stereo Microphone" });
    await expect
      .poll(() => page.evaluate(() => (window as unknown as TestWindow).__tauriLastArgs["set_voice_input_device"]))
      .toEqual({ deviceName: YETI });
    expect((await savedDevices(page)).input).toBe(YETI);
  });

  test("a value an older release saved resolves to what it names now, in storage only", async ({ page }) => {
    // Linux saved ALSA plugin names; macOS/Windows saved device names.
    await openVoiceSettings(page, { input: "pulse", output: "AirPods Pro" });
    await expect(page.getByTestId("voice-input-select")).toHaveValue("default");
    await expect(page.getByTestId("voice-output-select")).toHaveValue(AIRPODS);
    await expect.poll(() => savedDevices(page)).toEqual({ input: "default", output: AIRPODS });
    expect(await invokeCount(page, "set_voice_input_device")).toBe(0);
    expect(await invokeCount(page, "set_voice_output_device")).toBe(0);
  });

  test("a saved device that is away shows the default and stays saved", async ({ page }) => {
    const away = "pulseaudio:bluez_input.AA_BB_CC_DD_EE_FF.0";
    await openVoiceSettings(page, { input: away });
    await expect(page.getByTestId("voice-input-select")).toHaveValue("default");
    expect((await savedDevices(page)).input).toBe(away);
  });

  test("a device change from Rust refetches the list", async ({ page }) => {
    await openVoiceSettings(page);
    await expect.poll(() => invokeCount(page, "list_audio_devices")).toBeGreaterThan(0);
    const before = await invokeCount(page, "list_audio_devices");
    await page.evaluate(() =>
      (window as unknown as TestWindow).__pollisVoice.injectEvent({ type: "audio_devices_changed" }),
    );
    await expect.poll(() => invokeCount(page, "list_audio_devices")).toBeGreaterThan(before);
  });
});
