import React from "react";
import { CopyDiagnosticsButton } from "../Diagnostics/CopyDiagnosticsButton";

interface AuthErrorProps {
  message: string;
}

/// A sign-in error, with the way to get the details to someone (#1305): the
/// text alone is often not enough to tell which step failed, and before a
/// session exists the Diagnostics page is out of reach.
export const AuthError: React.FC<AuthErrorProps> = ({ message }) => (
  <div className="flex flex-col items-start gap-2">
    <p data-testid="auth-error" className="text-xs font-mono text-danger">
      {message}
    </p>
    <CopyDiagnosticsButton size="xs" variant="ghost" />
  </div>
);
