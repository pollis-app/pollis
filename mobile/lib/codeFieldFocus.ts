// Reopens the soft keyboard for a hidden code input (the OTP screen's six
// boxes are a picture of an opacity:0 TextInput). Pure, so the node tests
// reach it.
//
// React Native's `focus()` is a no-op when the input is already the focused
// one (`TextInputState.focusTextInput`). Dismissing the keyboard with Android's
// back gesture/button hides it WITHOUT blurring the input, so a plain
// `focus()` on the next tap did nothing and the user was stuck. When the field
// is focused but the keyboard is down, blur first so `focus()` reaches native
// and shows the keyboard again.

export interface FocusableInput {
  isFocused(): boolean;
  blur(): void;
  focus(): void;
}

export function reopenKeyboard(input: FocusableInput | null | undefined, keyboardVisible: boolean): void {
  if (!input) {
    return;
  }
  if (input.isFocused()) {
    if (keyboardVisible) {
      return;
    }
    input.blur();
  }
  input.focus();
}
