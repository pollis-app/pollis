// The on-device diagnostics log (#1305). pollis-core writes it under the app's
// data directory; this reads it back with a short header and puts it on the
// clipboard for the user to send. Nothing here touches the network.
import Constants from "expo-constants";
import * as Clipboard from "expo-clipboard";
import { invoke } from "./native";
import { haptic } from "./haptics";

/** The report "Copy diagnostics" copies — the header names this build. */
export function getDiagnostics(): Promise<string> {
  return invoke<string>("get_diagnostics", {
    appVersion: Constants.expoConfig?.version ?? null,
  });
}

/** Copy the report; true when it reached the clipboard. */
export async function copyDiagnostics(): Promise<boolean> {
  let ok = false;
  try {
    ok = await Clipboard.setStringAsync(await getDiagnostics());
  } catch {
    ok = false;
  }
  // A copy confirms by touch, success or not (#1286).
  haptic(ok ? "success" : "error");
  return ok;
}
