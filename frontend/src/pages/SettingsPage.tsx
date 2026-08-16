import { errorMessage } from "../utils/errorMessage";
import React, { useState, useEffect, useCallback, useRef } from "react";
import { Trans, useTranslation } from "react-i18next";
import { Upload, User } from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import { PageShell } from "../components/Layout/PageShell";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { uploadAvatar } from "../services/r2-upload";
import { resizeImage } from "../utils/imageProcessing";
import { useUserProfile, useUpdateProfile, useUpdateAvatar, useUserAvatar, userQueryKeys } from "../hooks/queries";
import { TextInput } from "../components/ui/TextInput";
import { Button } from "../components/ui/Button";
import { convertFileSrc, invoke } from "../bridge";
import { EmptyState } from "../components/ui/EmptyState";

export const SettingsPage: React.FC = observer(() => {
  const { t } = useTranslation("settings");
  const { currentUser } = appStore;

  const { data: userData, isLoading } = useUserProfile();
  const { data: avatarDownloadUrl } = useUserAvatar();
  const updateProfileMutation = useUpdateProfile();
  const updateAvatarMutation = useUpdateAvatar();

  const [selectedFile, setSelectedFile] = useState<File | null>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const [currentAvatarUrl, setCurrentAvatarUrl] = useState<string | null>(null);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [username, setUsername] = useState("");
  const [preferredName, setPreferredName] = useState("");
  const [email, setEmail] = useState("");
  const [phone, setPhone] = useState("");

  // Email-change flow lives entirely in this component:
  //   'idle'    → display current email + "Change" button
  //   'request' → input new address, send OTP
  //   'verify'  → input the OTP, swap email
  // Distinct mutations would obscure the linear UX, so we hold state locally.
  const [emailChangeStep, setEmailChangeStep] = useState<"idle" | "request" | "verify">("idle");
  const [pendingNewEmail, setPendingNewEmail] = useState("");
  const [emailOtpCode, setEmailOtpCode] = useState("");
  const [emailChangeError, setEmailChangeError] = useState<string | null>(null);
  const [emailChangePending, setEmailChangePending] = useState(false);
  const queryClient = useQueryClient();
  const [fileInputKey, setFileInputKey] = useState(0);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [saveSuccess, setSaveSuccess] = useState(false);

  useEffect(() => {
    return () => { if (preview) { URL.revokeObjectURL(preview); } };
  }, [preview]);

  useEffect(() => {
    if (userData) {
      setUsername(userData.username || "");
      setPreferredName(userData.preferred_name || "");
      setEmail(userData.email || "");
      setPhone(userData.phone || "");
    }
  }, [userData]);

  useEffect(() => { setCurrentAvatarUrl(avatarDownloadUrl || null); }, [avatarDownloadUrl]);

  // Accept image files dropped anywhere on the window while on this page.
  useEffect(() => {
    const handlePathDrop = (e: Event) => {
      const paths: string[] = (e as CustomEvent<{ paths: string[] }>).detail?.paths ?? [];
      const imagePath = paths.find((p) => /\.(png|jpe?g|gif|webp|avif|svg)$/i.test(p));
      if (!imagePath) {
        return;
      }
      // Convert the native path to a File-like object via fetch(convertFileSrc(path)).
      // Tauri exposes native paths through the asset protocol; Electron uses
      // the custom `pollis-file://` protocol registered in main.
      const src = convertFileSrc(imagePath);
      fetch(src)
        .then((r) => r.blob())
        .then((blob) => {
          const name = imagePath.split(/[\\/]/).pop() ?? "image";
          const file = new File([blob], name, { type: blob.type || "image/png" });
          setSelectedFile(file);
          setUploadError(null);
          if (preview) {
            URL.revokeObjectURL(preview);
          }
          setPreview(URL.createObjectURL(file));
        })
        .catch((err) => {
          console.error("[Settings] pathdrop read failed:", err);
        });
    };

    window.addEventListener("pollis:pathdrop", handlePathDrop);
    return () => window.removeEventListener("pollis:pathdrop", handlePathDrop);
  }, [preview]);

  const handleFileChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) {
      setSelectedFile(null);
      if (preview) { URL.revokeObjectURL(preview); }
      setPreview(null);
      setUploadError(null);
      return;
    }
    setSelectedFile(file);
    setUploadError(null);
    if (preview) { URL.revokeObjectURL(preview); }
    if (file.type.startsWith("image/")) { setPreview(URL.createObjectURL(file)); }
  };

  const handleAvatarUpload = useCallback(async () => {
    if (!selectedFile || !currentUser) { return; }
    setUploadError(null);
    try {
      const optimizedFile = await resizeImage(selectedFile);
      const response = await uploadAvatar(currentUser.id, "", optimizedFile);
      await updateAvatarMutation.mutateAsync(response.object_key);
      setSelectedFile(null);
      if (preview) { URL.revokeObjectURL(preview); }
      setPreview(null);
      setFileInputKey((prev) => prev + 1);
      setSaveSuccess(true);
    } catch (error) {
      setUploadError(errorMessage(error, t("user.avatarUploadFailed")));
    }
  }, [selectedFile, currentUser, preview, updateAvatarMutation, t]);

  useEffect(() => {
    if (saveSuccess) {
      const t = setTimeout(() => setSaveSuccess(false), 3000);
      return () => clearTimeout(t);
    }
  }, [saveSuccess]);

  const handleSave = async () => {
    if (!currentUser) { return; }
    try {
      await updateProfileMutation.mutateAsync({
        username: username.trim(),
        preferredName: preferredName.trim() || undefined,
        phone: phone.trim() || undefined,
      });
      setSaveSuccess(true);
      setTimeout(() => setSaveSuccess(false), 3000);
    } catch (error) {
      console.error("Failed to save settings:", error);
    }
  };

  const cancelEmailChange = () => {
    setEmailChangeStep("idle");
    setPendingNewEmail("");
    setEmailOtpCode("");
    setEmailChangeError(null);
  };

  const handleSendEmailChangeOtp = async () => {
    if (!currentUser) { return; }
    const target = pendingNewEmail.trim();
    if (!target) {
      setEmailChangeError(t("user.newEmailRequired"));
      return;
    }
    setEmailChangePending(true);
    setEmailChangeError(null);
    try {
      await invoke("request_email_change_otp", { userId: currentUser.id, newEmail: target });
      setEmailChangeStep("verify");
    } catch (err) {
      setEmailChangeError(errorMessage(err));
    } finally {
      setEmailChangePending(false);
    }
  };

  const handleVerifyEmailChange = async () => {
    if (!currentUser) { return; }
    if (!emailOtpCode.trim()) {
      setEmailChangeError(t("user.verificationCodeRequired"));
      return;
    }
    setEmailChangePending(true);
    setEmailChangeError(null);
    try {
      await invoke("verify_email_change", {
        userId: currentUser.id,
        newEmail: pendingNewEmail.trim(),
        code: emailOtpCode.trim(),
      });
      // Refetch the profile so the displayed email updates without a reload.
      await queryClient.invalidateQueries({ queryKey: userQueryKeys.profile(currentUser.id) });
      cancelEmailChange();
    } catch (err) {
      setEmailChangeError(errorMessage(err));
    } finally {
      setEmailChangePending(false);
    }
  };

  if (!currentUser) {
    return (
      <PageShell title={t("user.title")} scrollable>
        <EmptyState testId="settings-no-user">{t("user.signInPrompt")}</EmptyState>
      </PageShell>
    );
  }

  return (
    <PageShell title={t("user.title")} scrollable>
      <div
        data-testid="settings-page"
        className="flex-1 flex flex-col overflow-auto"
        style={{ background: 'var(--c-bg)' }}
      >
        <div data-testid="settings-content" className="flex-1 flex justify-center overflow-auto px-6 py-8">
          <div className="w-full max-w-md flex flex-col gap-8">

            {/* Account */}
            <section className="flex flex-col gap-4 mb-12">
              <h2 className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b" style={{ color: 'var(--c-text-dim)', borderColor: 'var(--c-border)' }}>
                {t("user.accountHeading")}
              </h2>

              {isLoading ? (
                <span data-testid="settings-loading" className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>
                  {t("common:states.loading")}
                </span>
              ) : (
                <div className="flex flex-col gap-4">
                  <TextInput
                    label={t("user.usernameLabel")}
                    value={username}
                    onChange={setUsername}
                    placeholder={t("user.usernamePlaceholder")}
                    id="settings-username"
                  />
                  <input data-testid="settings-username-input" type="hidden" value={username} readOnly />

                  <TextInput
                    label={t("user.preferredNameLabel")}
                    value={preferredName}
                    onChange={setPreferredName}
                    placeholder={t("user.preferredNamePlaceholder")}
                    id="settings-preferred-name"
                  />
                  <input data-testid="settings-preferred-name-input" type="hidden" value={preferredName} readOnly />

                  {/* Phone field hidden — not currently surfaced as a feature.
                      State and backend wiring are intentionally left in place
                      so re-enabling is a one-uncomment change.
                  <TextInput
                    label="Phone"
                    value={phone}
                    onChange={setPhone}
                    placeholder="+1 555 000 0000"
                    id="settings-phone"
                  />
                  <input data-testid="settings-phone-input" type="hidden" value={phone} readOnly />
                  */}
                </div>
              )}

              {updateProfileMutation.error && (
                <p data-testid="settings-save-error" className="text-xs font-mono" style={{ color: 'var(--c-danger)' }}>
                  {updateProfileMutation.error instanceof Error
                    ? updateProfileMutation.error.message
                    : t("user.saveFailed")}
                </p>
              )}

              {saveSuccess && (
                <p data-testid="settings-save-success" className="text-xs font-mono" style={{ color: 'var(--c-accent-dim)' }}>
                  {t("user.saved")}
                </p>
              )}

              <Button
                data-testid="settings-save-button"
                onClick={handleSave}
                disabled={updateProfileMutation.isPending}
                isLoading={updateProfileMutation.isPending}
                loadingText={t("user.saving")}
              >
                {t("user.saveButton")}
              </Button>
            </section>

            {/* Email is its own section so the user doesn't mistake it for
                something the "Save Changes" button covers — email only mutates
                via the OTP-verified flow below. */}
            <section className="flex flex-col gap-4 mb-12">
              <h2 className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b" style={{ color: 'var(--c-text-dim)', borderColor: 'var(--c-border)' }}>
                {t("user.emailHeading")}
              </h2>

              {isLoading ? (
                <span className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>
                  {t("common:states.loading")}
                </span>
              ) : emailChangeStep === "idle" ? (
                <div className="flex flex-col gap-1.5">
                  <TextInput
                    label={t("user.emailLabel")}
                    value={email}
                    onChange={() => { /* read-only — change via the button below */ }}
                    type="email"
                    placeholder={t("user.emailPlaceholder")}
                    id="settings-email"
                    disabled
                  />
                  <input data-testid="settings-email-input" type="hidden" value={email} readOnly />
                  <Button
                    data-testid="settings-email-change-button"
                    variant="secondary"
                    size="sm"
                    onClick={() => {
                      setPendingNewEmail("");
                      setEmailOtpCode("");
                      setEmailChangeError(null);
                      setEmailChangeStep("request");
                    }}
                    className="self-start mt-3"
                  >
                    {t("user.changeEmailButton")}
                  </Button>
                </div>
              ) : emailChangeStep === "request" ? (
                <div className="flex flex-col gap-2">
                  <TextInput
                    label={t("user.newEmailLabel")}
                    value={pendingNewEmail}
                    onChange={setPendingNewEmail}
                    type="email"
                    placeholder={t("user.emailPlaceholder")}
                    id="settings-email-new"
                    disabled={emailChangePending}
                  />
                  <input data-testid="settings-email-new-input" type="hidden" value={pendingNewEmail} readOnly />
                  {emailChangeError && (
                    <p data-testid="settings-email-change-error" className="text-xs font-mono" style={{ color: "var(--c-danger)" }}>
                      {emailChangeError}
                    </p>
                  )}
                  <div className="flex gap-2">
                    <Button
                      data-testid="settings-email-send-code"
                      size="sm"
                      onClick={handleSendEmailChangeOtp}
                      isLoading={emailChangePending}
                      loadingText={t("user.sending")}
                    >
                      {t("user.sendCodeButton")}
                    </Button>
                    <Button
                      data-testid="settings-email-cancel"
                      variant="ghost"
                      size="sm"
                      onClick={cancelEmailChange}
                      disabled={emailChangePending}
                    >
                      {t("common:actions.cancel")}
                    </Button>
                  </div>
                </div>
              ) : (
                <div className="flex flex-col gap-2">
                  <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                    <Trans
                      t={t}
                      i18nKey="user.codeSent"
                      values={{ email: pendingNewEmail }}
                      components={{ addr: <span style={{ color: "var(--c-text)" }} /> }}
                    />
                  </p>
                  <TextInput
                    label={t("user.verificationCodeLabel")}
                    value={emailOtpCode}
                    onChange={setEmailOtpCode}
                    placeholder="000000"
                    id="settings-email-otp"
                    disabled={emailChangePending}
                  />
                  <input data-testid="settings-email-otp-input" type="hidden" value={emailOtpCode} readOnly />
                  {emailChangeError && (
                    <p data-testid="settings-email-change-error" className="text-xs font-mono" style={{ color: "var(--c-danger)" }}>
                      {emailChangeError}
                    </p>
                  )}
                  <div className="flex gap-2">
                    <Button
                      data-testid="settings-email-verify"
                      size="sm"
                      onClick={handleVerifyEmailChange}
                      isLoading={emailChangePending}
                      loadingText={t("user.verifying")}
                    >
                      {t("user.verifyButton")}
                    </Button>
                    <Button
                      data-testid="settings-email-back"
                      variant="ghost"
                      size="sm"
                      onClick={() => {
                        setEmailOtpCode("");
                        setEmailChangeError(null);
                        setEmailChangeStep("request");
                      }}
                      disabled={emailChangePending}
                    >
                      {t("common:actions.back")}
                    </Button>
                  </div>
                </div>
              )}
            </section>

            {/* Avatar */}
            <section className="flex flex-col gap-4 mb-12">
              <h2 className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b" style={{ color: 'var(--c-text-dim)', borderColor: 'var(--c-border)' }}>
                {t("user.avatarHeading")}
              </h2>

              <div className="flex items-center gap-4">
                <div
                  data-testid="avatar-preview-container"
                  className="w-14 h-14 overflow-hidden flex items-center justify-center flex-shrink-0 cursor-pointer rounded-panel"
                  style={{ border: '2px solid var(--c-border)', background: 'var(--c-surface-high)' }}
                  onClick={() => fileInputRef.current?.click()}
                  title={t("user.avatarPickTitle")}
                >
                  {preview ? (
                    <img data-testid="avatar-new-preview" src={preview} alt={t("user.avatarPreviewAlt")} className="w-full h-full object-cover" />
                  ) : currentAvatarUrl ? (
                    <img
                      data-testid="avatar-current"
                      src={currentAvatarUrl}
                      alt={t("user.avatarAlt")}
                      className="w-full h-full object-cover"
                      onError={() => setCurrentAvatarUrl(null)}
                    />
                  ) : (
                    <User data-testid="avatar-placeholder" size={22} aria-hidden="true" style={{ color: 'var(--c-text-muted)' }} />
                  )}
                </div>

                <div className="flex flex-col gap-2">
                  <label
                    htmlFor="settings-avatar-input"
                    className="inline-flex items-center gap-1.5 text-xs font-mono cursor-pointer transition-colors"
                    style={{ color: 'var(--c-accent)' }}
                  >
                    <Upload size={14} aria-hidden="true" />
                    {t("user.chooseImage")}
                  </label>
                  <input
                    key={fileInputKey}
                    ref={fileInputRef}
                    id="settings-avatar-input"
                    data-testid="settings-avatar-input"
                    type="file"
                    accept="image/*"
                    onChange={handleFileChange}
                    disabled={updateAvatarMutation.isPending}
                    aria-label={t("user.avatarInputLabel")}
                    className="sr-only"
                  />
                  <p className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>
                    {t("user.avatarHint")}
                  </p>
                </div>
              </div>

              {uploadError && (
                <p data-testid="avatar-upload-error" className="text-xs font-mono" style={{ color: 'var(--c-danger)' }}>
                  {uploadError}
                </p>
              )}

              {saveSuccess && !selectedFile && (
                <p data-testid="avatar-upload-success" className="text-xs font-mono" style={{ color: 'var(--c-accent-dim)' }}>
                  {t("user.avatarUpdated")}
                </p>
              )}

              {selectedFile && (
                <Button
                  data-testid="upload-avatar-button"
                  onClick={handleAvatarUpload}
                  disabled={updateAvatarMutation.isPending}
                  isLoading={updateAvatarMutation.isPending}
                  loadingText={t("user.uploading")}
                >
                  {t("user.uploadAvatarButton")}
                </Button>
              )}
            </section>

          </div>
        </div>
      </div>
    </PageShell>
  );
});
