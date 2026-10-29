import React from "react";
import { CopyDiagnosticsButton } from "../Diagnostics/CopyDiagnosticsButton";

interface AuthErrorProps {
  message: string;
}

/// A sign-in error plus a diagnostics copy button (#1305): Diagnostics is unreachable before a session.
export const AuthError: React.FC<AuthErrorProps> = ({ message }) => (
  <div className="flex flex-col items-start gap-2">
    <p data-testid="auth-error" className="text-xs font-mono text-danger">
      {message}
    </p>
    <CopyDiagnosticsButton size="xs" variant="ghost" />
  </div>
);
