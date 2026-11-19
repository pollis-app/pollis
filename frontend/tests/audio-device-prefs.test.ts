// Saved mic/speaker choices resolve against the listed devices (#1989).

import { test } from "node:test";
import assert from "node:assert/strict";
import { resolveSavedDevice, shownDevice } from "../src/voice/devicePrefs.ts";

const linux = [
  { id: "pulseaudio:alsa_input.pci-0000_00_1f.3.analog-stereo", name: "Built-in Audio Analog Stereo" },
  { id: "pulseaudio:bluez_input.AA_BB_CC_DD_EE_FF.0", name: "AirPods Pro" },
];
const mac = [
  { id: "coreaudio:BuiltInMicrophoneDevice", name: "MacBook Pro Microphone" },
  { id: "coreaudio:AA-BB-CC:input", name: "Dan's AirPods Pro" },
];

test("a listed id and the default stay as they are", () => {
  assert.equal(resolveSavedDevice("pulseaudio:bluez_input.AA_BB_CC_DD_EE_FF.0", linux), "pulseaudio:bluez_input.AA_BB_CC_DD_EE_FF.0");
  assert.equal(resolveSavedDevice("default", linux), "default");
});

test("pre-#1989 Linux values (ALSA plugin names) become the default", () => {
  for (const legacy of ["pulse", "pipewire", "jack", "hw:CARD=QuadCast,DEV=0"]) {
    assert.equal(resolveSavedDevice(legacy, linux), "default", legacy);
  }
});

test("pre-#1989 macOS/Windows values (device names) keep their device under its new id", () => {
  assert.equal(resolveSavedDevice("Dan's AirPods Pro", mac), "coreaudio:AA-BB-CC:input");
  assert.equal(resolveSavedDevice("USB Audio CODEC", mac), "default");
});

test("an id whose device is away stays saved, and the picker shows the default meanwhile", () => {
  const away = "pulseaudio:alsa_input.usb-Yeti";
  assert.equal(resolveSavedDevice(away, linux), away);
  assert.equal(shownDevice(away, linux), "default");
  assert.equal(shownDevice(linux[1].id, linux), linux[1].id);
});

test("an id never matches by name", () => {
  assert.equal(resolveSavedDevice("coreaudio:Gone", [{ id: "coreaudio:x", name: "coreaudio:Gone" }]), "coreaudio:Gone");
});

test("an empty list (enumeration failed) keeps the saved value", () => {
  assert.equal(resolveSavedDevice("Dan's AirPods Pro", []), "Dan's AirPods Pro");
});
