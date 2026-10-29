// Reopens the soft keyboard for a hidden code input (the OTP boxes). RN's
// `focus()` is a no-op on the focused input, and Android's back gesture hides
// the keyboard WITHOUT blurring, so blur first when the keyboard is down.

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
