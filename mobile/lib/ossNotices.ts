import type { OssNotices, OssNoticePackage } from "../types/ossNotices";

// Generated from the lockfiles by scripts/oss-inventory.mjs when Metro starts
// (metro.config.js), so the list matches what this build ships (#1832). The
// file is ~1 MB, so it is required on first use, not at app start.
let cached: OssNotices | null = null;

export function ossNotices(): OssNotices {
  if (!cached) {
    cached = require("../generated/oss-notices.json") as OssNotices;
  }
  return cached;
}

export function ossPackageId(p: OssNoticePackage): string {
  return `${p.ecosystem}:${p.name}@${p.version}`;
}
