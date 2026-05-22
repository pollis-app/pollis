import React from "react";
import { ShieldCheck, Lock, RefreshCw } from "lucide-react";
import { Button } from "../ui/Button";
import { Switch } from "../ui/Switch";

export interface VerifiedContact {
  id: string;
  name: string;
  fingerprint: string;
  verifiedAt: number;
}

export interface SessionInfo {
  contactId: string;
  contactName: string;
  sessionId: string;
  lastRatchetAt: number;
  status: "active" | "stale" | "warning";
}

interface SecuritySettingsProps {
  ownFingerprint: string;
  verifiedContacts?: VerifiedContact[];
  sessions?: SessionInfo[];
  messagePreviewsEnabled?: boolean;
  onToggleMessagePreviews?: (enabled: boolean) => void;
  onClearSessions?: () => void;
  onResetSession?: (sessionId: string) => void;
}

const formatFingerprint = (fp: string) => (fp.match(/.{1,5}/g) || []).join(" ");

export const SecuritySettings: React.FC<SecuritySettingsProps> = ({
  ownFingerprint,
  verifiedContacts = [],
  sessions = [],
  messagePreviewsEnabled = false,
  onToggleMessagePreviews,
  onClearSessions,
  onResetSession,
}) => {
  return (
    <div
      data-testid="security-settings"
      className="flex flex-col gap-8"
      style={{ background: 'var(--c-bg)' }}
    >
      {/* Safety number */}
      <section className="flex flex-col gap-3">
        <h2 className="section-label px-0 border-b pb-1" style={{ borderColor: 'var(--c-border)' }}>
          <span className="inline-flex items-center gap-1.5"><Lock size={17} aria-hidden="true" />Identity</span>
        </h2>
        <div className="flex flex-col gap-0.5">
          <p className="text-2xs font-mono uppercase tracking-widest" style={{ color: 'var(--c-text-muted)' }}>Your safety number</p>
          <p className="text-xs font-mono" style={{ color: 'var(--c-accent-dim)' }}>{formatFingerprint(ownFingerprint)}</p>
        </div>
      </section>

      {/* Message previews */}
      <section className="flex flex-col gap-3">
        <h2 className="section-label px-0 border-b pb-1" style={{ borderColor: 'var(--c-border)' }}>Notifications</h2>
        <Switch
          data-testid="message-previews-toggle"
          label="Message previews"
          description="Show decrypted previews in notifications — may leak on lock screen"
          checked={messagePreviewsEnabled}
          onChange={(checked) => onToggleMessagePreviews?.(checked)}
        />
      </section>

      {/* Verified contacts */}
      <section className="flex flex-col gap-3">
        <h2 className="section-label px-0 border-b pb-1" style={{ borderColor: 'var(--c-border)' }}>
          Verified Contacts
          <span className="ml-2 font-normal" style={{ color: 'var(--c-text-muted)' }}>{verifiedContacts.length}</span>
        </h2>
        {verifiedContacts.length === 0 ? (
          <p className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>No verified contacts yet.</p>
        ) : (
          <div className="flex flex-col divide-y" style={{ borderColor: 'var(--c-border)' }}>
            {verifiedContacts.map((c) => (
              <div key={c.id} data-testid={`verified-contact-${c.id}`} className="flex items-center justify-between py-2 gap-3">
                <div className="flex flex-col gap-0.5 min-w-0">
                  <p className="text-xs font-mono" style={{ color: 'var(--c-text-dim)' }}>{c.name}</p>
                  <p className="text-2xs font-mono truncate" style={{ color: 'var(--c-text-muted)' }}>{formatFingerprint(c.fingerprint)}</p>
                </div>
                <span className="inline-flex items-center gap-1 text-2xs font-mono flex-shrink-0" style={{ color: 'var(--c-accent)' }}>
                  <ShieldCheck size={19} aria-hidden="true" />
                  verified
                </span>
              </div>
            ))}
          </div>
        )}
      </section>

      {/* Sessions */}
      <section className="flex flex-col gap-3">
        <h2 className="section-label px-0 border-b pb-1" style={{ borderColor: 'var(--c-border)' }}>Sessions</h2>
        {sessions.length === 0 ? (
          <p className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>No active sessions.</p>
        ) : (
          <div className="flex flex-col divide-y" style={{ borderColor: 'var(--c-border)' }}>
            {sessions.map((s) => {
              const statusColor = s.status === 'active' ? 'var(--c-accent)' : s.status === 'warning' ? '#f0b429' : 'var(--c-text-muted)';
              return (
                <div key={s.sessionId} data-testid={`session-${s.sessionId}`} className="flex items-center justify-between py-2 gap-3">
                  <div className="flex flex-col gap-0.5 min-w-0">
                    <p className="text-xs font-mono" style={{ color: 'var(--c-text-dim)' }}>{s.contactName}</p>
                    <p className="text-2xs font-mono" style={{ color: 'var(--c-text-muted)' }}>
                      {new Date(s.lastRatchetAt).toLocaleString()}
                    </p>
                  </div>
                  <div className="flex items-center gap-2 flex-shrink-0">
                    <span className="text-2xs font-mono" style={{ color: statusColor }}>{s.status}</span>
                    <button
                      data-testid={`reset-session-${s.sessionId}`}
                      onClick={() => onResetSession?.(s.sessionId)}
                      className="icon-btn-sm"
                      aria-label="Reset session"
                    >
                      <RefreshCw size={17} aria-hidden="true" />
                    </button>
                  </div>
                </div>
              );
            })}
          </div>
        )}
        <Button
          data-testid="clear-all-sessions-button"
          onClick={onClearSessions}
          variant="ghost"
          className="self-start"
        >
          Clear all sessions
        </Button>
      </section>
    </div>
  );
};
