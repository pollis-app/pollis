/// <reference types="vite/client" />

interface ImportMetaEnv {
  // Set to "true" when building for the Mac App Store. Used to hide updater
  // UI and avoid statically importing `@tauri-apps/plugin-updater` /
  // `@tauri-apps/plugin-process`, both of which are compiled out of MAS
  // builds via the Rust `mas` Cargo feature.
  readonly VITE_MAS_BUILD?: string;
  // Set by the Playwright harness so the frontend can short-circuit Tauri
  // invoke() calls it would otherwise make against a real backend.
  readonly VITE_PLAYWRIGHT?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
