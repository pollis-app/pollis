// The on-device diagnostics log (#1305), read back from pollis-core and put on
// the clipboard for the user to send. Nothing here touches the network.
import Constants from "expo-constants";
import * as Clipboard from "expo-clipboard";
import { invoke } from "./native";
import { haptic } from "./haptics";

export function getDiagnostics(): Promise<string> {
  return invoke<string>("get_diagnostics", {
    appVersion: Constants.expoConfig?.version ?? null,
  });
}

export async function copyDiagnostics(): Promise<boolean> {
  let ok = false;
  try {
    ok = await Clipboard.setStringAsync(await getDiagnostics());
  } catch {
    ok = false;
  }
  haptic(ok ? "success" : "error");
  return ok;
}
