// Reopens the keyboard for the hidden OTP input: `focus()` is a no-op on an already-focused input, so blur first. Pure.

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
