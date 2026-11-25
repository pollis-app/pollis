// #2099: whether a key event belongs to an input method's composition (Japanese, Chinese,
// Korean, dictation, the macOS accent menu). Such an Enter commits the composed text; acting on
// it as "send" or "save" would take the text before the commit, which then refills the field.
// WebKit (the Tauri webview on macOS and Linux) fires the committing Enter after
// compositionend, so `isComposing` is already false there and only keyCode 229 tells.
export function isImeKey(e: { nativeEvent: { isComposing?: boolean; keyCode?: number } }): boolean {
  return e.nativeEvent.isComposing === true || e.nativeEvent.keyCode === 229;
}
