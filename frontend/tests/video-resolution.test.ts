/*
 * The outgoing screen-share and camera resolution presets (#1902): what a stored value snaps to.
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  CAMERA_RESOLUTION_DEFAULT,
  CAMERA_RESOLUTION_OPTIONS,
  clampCameraResolution,
  clampScreenShareResolution,
  SCREEN_SHARE_RESOLUTION_DEFAULT,
  SCREEN_SHARE_RESOLUTION_OPTIONS,
  snapToResolutionPreset,
} from "../src/voice/videoResolution.ts";

test("every preset is a fixed point, so a stored choice reads back as itself", () => {
  for (const h of SCREEN_SHARE_RESOLUTION_OPTIONS) {
    assert.equal(clampScreenShareResolution(h), h);
  }
  for (const h of CAMERA_RESOLUTION_OPTIONS) {
    assert.equal(clampCameraResolution(h), h);
  }
});

test("the defaults are presets: 1080p for a share, the camera's own size for a camera", () => {
  assert.ok((SCREEN_SHARE_RESOLUTION_OPTIONS as readonly number[]).includes(SCREEN_SHARE_RESOLUTION_DEFAULT));
  assert.ok((CAMERA_RESOLUTION_OPTIONS as readonly number[]).includes(CAMERA_RESOLUTION_DEFAULT));
  assert.equal(SCREEN_SHARE_RESOLUTION_DEFAULT, 1080);
  assert.equal(CAMERA_RESOLUTION_DEFAULT, 0);
});

test("an absent or invalid value falls back to the track's default", () => {
  for (const bad of [undefined, Number.NaN, Number.POSITIVE_INFINITY, -720]) {
    assert.equal(clampScreenShareResolution(bad), SCREEN_SHARE_RESOLUTION_DEFAULT);
    assert.equal(clampCameraResolution(bad), CAMERA_RESOLUTION_DEFAULT);
  }
});

test("zero stays 'source' and an off-preset height snaps to the nearest real one", () => {
  assert.equal(clampScreenShareResolution(0), 0);
  assert.equal(clampCameraResolution(0), 0);
  assert.equal(clampScreenShareResolution(950), 1080);
  assert.equal(clampScreenShareResolution(4320), 1440);
  assert.equal(clampCameraResolution(480), 360);
  assert.equal(clampCameraResolution(600), 720);
  assert.equal(clampCameraResolution(2160), 1080);
  // A camera preset that is not a share preset is not carried across tracks.
  assert.equal(clampScreenShareResolution(360), 720);
});

test("the snap never returns 0 for a positive height", () => {
  assert.equal(snapToResolutionPreset(1, [720, 1080, 0], 1080), 720);
});
