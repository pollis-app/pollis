// Shape of scripts/oss-inventory.mjs `--notices` output (#1832). Mirrored in
// mobile/types/ossNotices.ts; bump `schema` in the generator on any change.
export interface OssNoticePackage {
  name: string;
  version: string;
  ecosystem: "cargo" | "npm";
  license: string;
  // Indexes into OssNotices.texts (identical licence texts are stored once).
  texts: number[];
}

export interface OssNotices {
  schema: 1;
  product: string;
  packages: OssNoticePackage[];
  texts: string[];
}
