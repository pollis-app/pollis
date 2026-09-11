// Metro config. Expo's defaults plus ONE extra watch folder: the desktop
// translation catalogues.
//
// `mobile/` is a standalone Expo project and deliberately imports nothing
// from `frontend/` (see mobile/CLAUDE.md) — generated data is COPIED across by
// its generator rather than shared. The catalogues are the one exception,
// because a copy of 1,300 translated strings across seven locales would drift
// the moment a desktop PR touched a key, and "which app is behind" is a
// question nobody should have to ask. `watchFolders` lets Metro resolve the
// JSON files under `frontend/src/i18n/locales/` from `mobile/i18n/resources.ts`;
// it does NOT make any frontend TypeScript importable, and nothing else in
// the frontend tree is watched.
//
// EAS Build and the CI Android job both bundle from a checkout of the whole
// repository, so the sibling directory is always present.

const { getDefaultConfig } = require("expo/metro-config");
const path = require("node:path");

const config = getDefaultConfig(__dirname);

config.watchFolders = [
  ...(config.watchFolders ?? []),
  path.resolve(__dirname, "../frontend/src/i18n/locales"),
];

module.exports = config;
