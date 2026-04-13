import React, { useState, useEffect, useRef } from 'react';
import { ArrowLeft } from 'lucide-react';
import * as api from '../../services/api';
import { Button } from '../ui/Button';
import { InputOtp } from '../ui/InputOtp';
import { TextInput } from '../ui/TextInput';

interface EmailOTPAuthProps {
  onSuccess: (result: api.AuthResult) => void | Promise<void>;
  // When set, auto-populates the email field and immediately sends the OTP,
  // advancing straight to the code-entry step.
  prefillEmail?: string;
  // Incremented each time the parent wants to trigger a new prefill, even for
  // the same email address (e.g. clicking the same chip after going back).
  prefillNonce?: number;
  // Called when the internal step changes so the parent can react (e.g. hide UI).
  onStepChange?: (step: 'email' | 'otp') => void;
}

export const EmailOTPAuth: React.FC<EmailOTPAuthProps> = ({ onSuccess, prefillEmail, prefillNonce, onStepChange }) => {
  const [step, setStepRaw] = useState<'email' | 'otp'>('email');
  const setStep = (s: 'email' | 'otp') => {
    setStepRaw(s);
    onStepChange?.(s);
  };
  const [email, setEmail] = useState('');
  const [otp, setOtp] = useState('');
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const hasAutoSubmittedRef = useRef(false);

  useEffect(() => {
    if (otp.length < 6) {
      hasAutoSubmittedRef.current = false;
      return;
    }
    if (hasAutoSubmittedRef.current || isLoading || error) {
      return;
    }
    hasAutoSubmittedRef.current = true;
    handleVerifyOTP();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [otp]);

  // Fire whenever the parent bumps the nonce (chip click). Skip nonce === 0
  // (initial mount) so we don't send a spurious request on page load.
  useEffect(() => {
    if (!prefillEmail || !prefillNonce) {
      return;
    }

    setEmail(prefillEmail);
    setStep('email');
    setOtp('');
    setError(null);
    setIsLoading(true);

    api.requestOTP(prefillEmail)
      .then(() => {
        setStep('otp');
      })
      .catch((err) => {
        setError(err instanceof Error ? err.message : 'Failed to send code');
      })
      .finally(() => {
        setIsLoading(false);
      });
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [prefillNonce]);

  const handleRequestOTP = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!email.trim()) {
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      await api.requestOTP(email.trim());
      setStep('otp');
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to send code');
    } finally {
      setIsLoading(false);
    }
  };

  const handleVerifyOTP = async (e?: React.FormEvent) => {
    e?.preventDefault();
    if (!otp.trim()) {
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      const result = await api.verifyOTP(email.trim(), otp.trim());
      await onSuccess(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Invalid code');
    } finally {
      setIsLoading(false);
    }
  };

  if (step === 'otp') {
    return (
      <div data-testid="otp-form-container" className="flex flex-col gap-4">
        <p className="text-xs font-mono" style={{ color: 'var(--c-text-dim)' }}>
          Code sent to <span style={{ color: 'var(--c-accent)' }}>{email}</span>
        </p>
        {error && (
          <p data-testid="auth-error" className="text-xs font-mono" style={{ color: '#ff6b6b' }}>
            {error}
          </p>
        )}
        <form data-testid="otp-form" onSubmit={handleVerifyOTP} className="flex flex-col gap-4">
          <div>
            <label className="block text-xs font-mono font-medium mb-2" style={{ color: 'var(--c-text-dim)' }}>
              Enter code
            </label>
            <InputOtp
              value={otp}
              onChange={setOtp}
              disabled={isLoading}
              autoFocus
            />
            {/* Preserve testid for E2E tests */}
            <input data-testid="otp-input" type="hidden" value={otp} readOnly />
          </div>
          <Button
            data-testid="verify-otp-button"
            type="submit"
            isLoading={isLoading}
            loadingText="Verifying…"
            disabled={otp.length < 6}
            className="w-full"
          >
            Verify
          </Button>
        </form>
        <button
          data-testid="back-to-email-button"
          onClick={() => { setStep('email'); setOtp(''); setError(null); }}
          className="inline-flex items-center gap-1 leading-none text-xs font-mono"
          style={{ color: 'var(--c-text-muted)' }}
        >
          <ArrowLeft size={14} /> Back
        </button>
      </div>
    );
  }

  return (
    <div data-testid="email-form-container" className="flex flex-col gap-4">
      {error && (
        <p data-testid="auth-error" className="text-xs font-mono" style={{ color: '#ff6b6b' }}>
          {error}
        </p>
      )}
      <form data-testid="email-form" onSubmit={handleRequestOTP} className="flex flex-col gap-4">
        <TextInput
          id="email-input"
          data-testid="email-input"
          label="Email"
          type="email"
          value={email}
          onChange={setEmail}
          placeholder="you@example.com"
          autoComplete="email"
          autoFocus
          disabled={isLoading}
          required
        />
        <Button
          data-testid="send-otp-button"
          type="submit"
          isLoading={isLoading}
          loadingText="Sending…"
          disabled={!email.trim()}
          className="w-full"
        >
          Continue
        </Button>
      </form>
    </div>
  );
};
