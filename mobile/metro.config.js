// Expo's defaults plus ONE watch folder: the shared translation catalogues (frontend/src/i18n/locales/). Nothing else in frontend/ is watched.

const { getDefaultConfig } = require("expo/metro-config");
const path = require("node:path");

const config = getDefaultConfig(__dirname);

config.watchFolders = [
  ...(config.watchFolders ?? []),
  path.resolve(__dirname, "../frontend/src/i18n/locales"),
];

module.exports = config;
