// The on-device diagnostics log (#1305), read back with a header: the newest part for the clipboard,
// the whole of it as a file for the share sheet. No network.
import Constants from "expo-constants";
import * as Clipboard from "expo-clipboard";
import * as FileSystem from "expo-file-system/legacy";
import * as Sharing from "expo-sharing";
import { invoke } from "./native";
import { haptic } from "./haptics";
import { diagnosticsFileName, recentReport } from "./diagnosticsReport";

/** OS-evictable, and emptied before each save, so at most one report sits on the device. */
const DIAGNOSTICS_DIR = `${FileSystem.cacheDirectory ?? ""}pollis-diagnostics/`;

export function getDiagnostics(): Promise<string> {
  return invoke<string>("get_diagnostics", {
    appVersion: Constants.expoConfig?.version ?? null,
  });
}

export async function copyDiagnostics(): Promise<boolean> {
  let ok = false;
  try {
    ok = await Clipboard.setStringAsync(recentReport(await getDiagnostics()).text);
  } catch {
    ok = false;
  }
  haptic(ok ? "success" : "error");
  return ok;
}

/** The whole report as a .txt through the share sheet (Files, Drive, a chat app); false when it could not be offered. */
export async function saveDiagnostics(dialogTitle: string): Promise<boolean> {
  try {
    if (!(await Sharing.isAvailableAsync())) {
      return false;
    }
    await FileSystem.deleteAsync(DIAGNOSTICS_DIR, { idempotent: true });
    await FileSystem.makeDirectoryAsync(DIAGNOSTICS_DIR, { intermediates: true });
    const uri = `${DIAGNOSTICS_DIR}${diagnosticsFileName()}`;
    await FileSystem.writeAsStringAsync(uri, await getDiagnostics());
    await Sharing.shareAsync(uri, { mimeType: "text/plain", UTI: "public.plain-text", dialogTitle });
    return true;
  } catch {
    return false;
  }
}
