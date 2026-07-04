export const isMac =
  typeof navigator !== "undefined" &&
  navigator.platform.toUpperCase().indexOf("MAC") >= 0;

export const isWindows =
  typeof navigator !== "undefined" &&
  navigator.userAgent.toLowerCase().includes("windows");

export const isLinux =
  typeof navigator !== "undefined" &&
  navigator.userAgent.toLowerCase().includes("linux") &&
  !isMac &&
  !isWindows;
