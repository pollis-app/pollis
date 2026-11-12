import { useEffect, useState } from "react";
import type { OssNotices, OssNoticePackage } from "../types/ossNotices";

// The notices are ~1 MB, so they load as their own chunk when a licences page
// opens, never with the app (#1832).
let pending: Promise<OssNotices | null> | null = null;

function load(): Promise<OssNotices | null> {
  if (!pending) {
    pending = import("virtual:oss-notices").then((m) => m.default);
  }
  return pending;
}

export type OssNoticesState =
  | { status: "loading" }
  | { status: "unavailable" }
  | { status: "ready"; notices: OssNotices };

export function useOssNotices(): OssNoticesState {
  const [state, setState] = useState<OssNoticesState>({ status: "loading" });
  useEffect(() => {
    let cancelled = false;
    load()
      .then((notices) => {
        if (!cancelled) {
          setState(notices ? { status: "ready", notices } : { status: "unavailable" });
        }
      })
      .catch(() => {
        if (!cancelled) {
          setState({ status: "unavailable" });
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);
  return state;
}

export function ossPackageId(p: OssNoticePackage): string {
  return `${p.ecosystem}:${p.name}@${p.version}`;
}
