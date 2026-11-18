import type { TFunction } from "i18next";
import { errorMessage } from "../../utils/errorMessage";
import { botErrorKind } from "../../utils/bots";

/** What a Bots page says about a failed pairing, add-host, revoke or delete (#1182). */
export function botErrorText(err: unknown, t: TFunction<"channels">, fallback: string): string {
  switch (botErrorKind(err)) {
    case "codeMismatch":
      return t("channels:bots.errors.codeMismatch");
    case "capReached":
      return t("channels:bots.errors.capReached");
    case "requestExpired":
      return t("channels:bots.errors.requestExpired");
    default:
      return errorMessage(err, fallback);
  }
}
