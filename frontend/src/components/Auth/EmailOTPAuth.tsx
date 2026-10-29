import { errorMessage } from '../../utils/errorMessage';
import React, { useState, useEffect, useRef } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { ArrowLeft } from 'lucide-react';
import { isMac, isLinux, isWindows } from '../../utils/platform';
import * as api from '../../services/api';
import { Button } from '../ui/Button';
import { InputOtp } from '../ui/InputOtp';
import { AuthError } from './AuthError';
import { TextInput } from '../ui/TextInput';
import { shellOpen } from '../../bridge';
import { OfferCode } from './OfferCode';

/// The name the signed-in device sees for this one.
function desktopDeviceName(): string {
  return isMac ? 'Pollis on macOS' : isWindows ? 'Pollis on Windows' : isLinux ? 'Pollis on Linux' : 'Pollis desktop';
}

interface EmailOTPAuthProps {
  onSuccess: (result: api.AuthResult) => void | Promise<void>;
  // Auto-fills the email and sends the OTP at once.
  prefillEmail?: string;
  // Bump to re-trigger a prefill, even for the same email.
  prefillNonce?: number;
  onStepChange?: (step: 'email' | 'otp') => void;
}

export const EmailOTPAuth: React.FC<EmailOTPAuthProps> = ({ onSuccess, prefillEmail, prefillNonce, onStepChange }) => {
  const { t } = useTranslation('auth');
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
  // This device shows a QR (#1361), or takes a pasted code (#1207).
  const [linkMode, setLinkMode] = useState(false);
  const [linkCode, setLinkCode] = useState('');

  const handleClaimLink = async (e?: React.FormEvent) => {
    e?.preventDefault();
    if (!linkCode.trim() || isLoading) {
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      const result = await api.claimDeviceLink(linkCode.trim(), desktopDeviceName());
      await onSuccess(result);
    } catch (err) {
      setError(errorMessage(err) || t('link.failed'));
    } finally {
      setIsLoading(false);
    }
  };

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

  // Skip nonce 0 (mount).
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
        setError(errorMessage(err, t('otp.sendFailed')));
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
      setError(errorMessage(err, t('otp.sendFailed')));
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
    // Only a failed verify means a wrong code; later failures must not say "Invalid code" (#1305).
    let result: api.AuthResult;
    try {
      result = await api.verifyOTP(email.trim(), otp.trim());
    } catch (err) {
      setError(errorMessage(err, t('otp.invalidCode')));
      setIsLoading(false);
      return;
    }
    try {
      await onSuccess(result);
    } catch (err) {
      setError(errorMessage(err, t('otp.signInFailed')));
    } finally {
      setIsLoading(false);
    }
  };

  if (step === 'otp') {
    return (
      <div data-testid="otp-form-container" className="flex flex-col gap-4">
        <p className="text-xs font-mono text-dim">
          <Trans
            t={t}
            i18nKey="otp.sentTo"
            values={{ email }}
            components={{ address: <span className="text-accent" /> }}
          />
        </p>
        {error && <AuthError message={error} />}
        <form data-testid="otp-form" onSubmit={handleVerifyOTP} className="flex flex-col gap-4">
          <div>
            <label className="block text-xs font-mono font-medium mb-2 text-dim">
              {t('otp.codeLabel')}
            </label>
            <InputOtp
              value={otp}
              onChange={setOtp}
              disabled={isLoading}
              autoFocus
            />
            <input data-testid="otp-input" type="hidden" value={otp} readOnly />
          </div>
          <Button
            data-testid="verify-otp-button"
            type="submit"
            isLoading={isLoading}
            loadingText={t('otp.verifying')}
            disabled={otp.length < 6}
            className="w-full mb-8"
          >
            {t('otp.verify')}
          </Button>
        </form>
        <button
          data-testid="back-to-email-button"
          onClick={() => { setStep('email'); setOtp(''); setError(null); }}
          className="inline-flex items-center gap-1 leading-none text-xs font-mono text-muted"
        >
          <ArrowLeft size={14} className="rtl-mirror" /> {t('common:actions.back')}
        </button>
      </div>
    );
  }

  if (linkMode) {
    return (
      <div data-testid="link-form-container" className="flex flex-col gap-6">
        <h2 className="text-base text-fg">{t('link.scanTitle')}</h2>
        <OfferCode deviceName={desktopDeviceName()} onSignedIn={onSuccess} />
        <div className="flex flex-col gap-2 pt-4 border-t border-line">
          <h3 className="text-sm text-fg">{t('link.pasteHeading')}</h3>
          <p className="text-sm text-muted leading-relaxed">{t('link.codeIntro')}</p>
        </div>
        {error && <AuthError message={error} />}
        <form data-testid="link-form" onSubmit={handleClaimLink} className="flex flex-col gap-4">
          <TextInput
            id="link-code-input"
            data-testid="link-code-input"
            label={t('link.codeLabel')}
            value={linkCode}
            onChange={setLinkCode}
            placeholder={t('link.codePlaceholder')}
            autoFocus
            disabled={isLoading}
            required
          />
          <Button data-testid="link-sign-in-button" type="submit" isLoading={isLoading} disabled={!linkCode.trim()} className="w-full">
            {t('link.signIn')}
          </Button>
        </form>
        <Button
          data-testid="link-back-to-email-button"
          variant="ghost"
          onClick={() => { setLinkMode(false); setLinkCode(''); setError(null); }}
        >
          {t('link.useEmail')}
        </Button>
      </div>
    );
  }

  return (
    <div data-testid="email-form-container" className="flex flex-col gap-4">
      {error && <AuthError message={error} />}
      <form data-testid="email-form" onSubmit={handleRequestOTP} className="flex flex-col gap-4">
        <TextInput
          id="email-input"
          data-testid="email-input"
          label={t('otp.emailLabel')}
          type="email"
          value={email}
          onChange={setEmail}
          placeholder={t('otp.emailPlaceholder')}
          autoComplete="email"
          autoFocus
          disabled={isLoading}
          required
        />
        <Button
          data-testid="send-otp-button"
          type="submit"
          isLoading={isLoading}
          loadingText={t('otp.sending')}
          disabled={!email.trim()}
          className="w-full"
        >
          {t('otp.continue')}
        </Button>
      </form>
      <Button
        data-testid="sign-in-with-device-button"
        variant="ghost"
        onClick={() => { setLinkMode(true); setError(null); }}
      >
        {t('link.useDevice')}
      </Button>
      {/* Agreeing to the terms is part of sign-up (#1213). */}
      <p data-testid="legal-agree" className="text-xs text-muted text-center leading-relaxed">
        <Trans
          t={t}
          i18nKey="legal.agree"
          components={{
            terms: (
              <button type="button" className="underline bg-transparent text-accent" onClick={() => void shellOpen('https://pollis.com/terms')} />
            ),
            privacy: (
              <button type="button" className="underline bg-transparent text-accent" onClick={() => void shellOpen('https://pollis.com/privacy')} />
            ),
          }}
        />
      </p>
    </div>
  );
};
