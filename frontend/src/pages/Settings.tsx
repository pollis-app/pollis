import React, { useState, useEffect, useCallback, useRef } from "react";
import { Upload, User } from "lucide-react";
import { useAppStore } from "../stores/appStore";
import { uploadAvatar } from "../services/r2-upload";
import { resizeImage } from "../utils/imageProcessing";
import { useUserProfile, useUpdateProfile, useUpdateAvatar, useUserAvatar } from "../hooks/queries";
import { TextInput } from "../components/ui/TextInput";
import { Button } from "../components/ui/Button";
import { getVersion } from "@tauri-apps/api/app";
import { check as checkForUpdate } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import * as api from "../services/api";

interface SettingsProps {
  onDeleteAccount?: () => void;
}

export const Settings: React.FC<SettingsProps> = ({ onDeleteAccount }) => {
  const { currentUser } = useAppStore();

  const { data: userData, isLoading } = useUserProfile();
  const { data: avatarDownloadUrl } = useUserAvatar();
  const updateProfileMutation = useUpdateProfile();
  const updateAvatarMutation = useUpdateAvatar();

  const [deleteConfirmText, setDeleteConfirmText] = useState("");
  const [isDeleting, setIsDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const [selectedFile, setSelectedFile] = useState<File | null>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const [currentAvatarUrl, setCurrentAvatarUrl] = useState<string | null>(null);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [username, setUsername] = useState("");
  const [email, setEmail] = useState("");
  const [phone, setPhone] = useState("");
  const [fileInputKey, setFileInputKey] = useState(0);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [appVersion, setAppVersion] = useState<string>("");
  const [updateStatus, setUpdateStatus] = useState<"idle" | "checking" | "available" | "none" | "error">("idle");
  const [updateVersion, setUpdateVersion] = useState<string>("");
  const [updateError, setUpdateError] = useState<string>("");
  const [isInstalling, setIsInstalling] = useState(false);

  useEffect(() => {
    return () => { if (preview) { URL.revokeObjectURL(preview); } };
  }, [preview]);

  useEffect(() => {
    if (userData) {
      setUsername(userData.username || "");
      setEmail(userData.email || "");
      setPhone(userData.phone || "");
    }
  }, [userData]);

  useEffect(() => {
    getVersion().then(setAppVersion).catch(() => setAppVersion("unknown"));
  }, []);

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
      // Tauri exposes native paths through the asset protocol.
      import("@tauri-apps/api/core").then(({ convertFileSrc }) => {
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
      setUploadError(error instanceof Error ? error.message : "Failed to upload avatar");
    }
  }, [selectedFile, currentUser, preview, updateAvatarMutation]);

  useEffect(() => {
    if (saveSuccess) {
      const t = setTimeout(() => setSaveSuccess(false), 3000);
      return () => clearTimeout(t);
    }
  }, [saveSuccess]);

  const handleSave = async () => {
    if (!currentUser) { return; }
    try {
      await updateProfileMutation.mutateAsync({ username: username.trim(), phone: phone.trim() || undefined });
      setSaveSuccess(true);
      setTimeout(() => setSaveSuccess(false), 3000);
    } catch (error) {
      console.error("Failed to save settings:", error);
    }
  };

  const handleDeleteAccount = useCallback(async () => {
    if (!currentUser) {
      return;
    }
    if (deleteConfirmText !== "DELETE") {
      return;
    }
    setIsDeleting(true);
    setDeleteError(null);
    try {
      await api.deleteAccount(currentUser.id);
      // Clear local state immediately so the user is logged out even if the
      // callback chain from the router context is broken.
      useAppStore.getState().logout();
      if (onDeleteAccount) {
        onDeleteAccount();
      } else {
        console.error("[Settings] onDeleteAccount callback is undefined — falling back to logout only");
      }
    } catch (error) {
      setDeleteError(error instanceof Error ? error.message : "Failed to delete account");
      setIsDeleting(false);
    }
  }, [currentUser, deleteConfirmText, onDeleteAccount]);

  const handleCheckForUpdates = useCallback(async () => {
    setUpdateStatus("checking");
    setUpdateError("");
    setUpdateVersion("");
    try {
      const update = await checkForUpdate();
      if (update) {
        setUpdateStatus("available");
        setUpdateVersion(update.version);
      } else {
        setUpdateStatus("none");
      }
    } catch (error) {
      setUpdateStatus("error");
      setUpdateError(error instanceof Error ? error.message : "Failed to check for updates");
    }
  }, []);

  const handleInstallUpdate = useCallback(async () => {
    if (updateStatus !== "available") {
      return;
    }
    setIsInstalling(true);
    try {
      const update = await checkForUpdate();
      if (!update) {
        return;
      }
      await update.downloadAndInstall();
      await relaunch();
    } catch (error) {
      setUpdateError(error instanceof Error ? error.message : "Failed to install update");
      setIsInstalling(false);
    }
  }, [updateStatus]);

  if (!currentUser) {
    return (
      <div data-testid="settings-no-user" className="flex items-center justify-center flex-1" style={{ background: 'var(--c-bg)' }}>
        <p className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>Please sign in</p>
      </div>
    );
  }

  return (
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
              Account
            </h2>

            {isLoading ? (
              <span data-testid="settings-loading" className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>
                Loading…
              </span>
            ) : (
              <div className="flex flex-col gap-4">
                <TextInput
                  label="Username"
                  value={username}
                  onChange={setUsername}
                  placeholder="username"
                  id="settings-username"
                />
                <input data-testid="settings-username-input" type="hidden" value={username} readOnly />

                <TextInput
                  label="Email"
                  value={email}
                  onChange={setEmail}
                  type="email"
                  placeholder="you@example.com"
                  id="settings-email"
                />
                <input data-testid="settings-email-input" type="hidden" value={email} readOnly />

                <TextInput
                  label="Phone"
                  value={phone}
                  onChange={setPhone}
                  placeholder="+1 555 000 0000"
                  id="settings-phone"
                />
                <input data-testid="settings-phone-input" type="hidden" value={phone} readOnly />
              </div>
            )}

            {updateProfileMutation.error && (
              <p data-testid="settings-save-error" className="text-xs font-mono" style={{ color: '#ff6b6b' }}>
                {updateProfileMutation.error instanceof Error
                  ? updateProfileMutation.error.message
                  : "Failed to save"}
              </p>
            )}

            {saveSuccess && (
              <p data-testid="settings-save-success" className="text-xs font-mono" style={{ color: 'var(--c-accent-dim)' }}>
                Saved.
              </p>
            )}

            <Button
              data-testid="settings-save-button"
              onClick={handleSave}
              disabled={updateProfileMutation.isPending}
              isLoading={updateProfileMutation.isPending}
              loadingText="Saving…"
            >
              Save Changes
            </Button>
          </section>

          {/* Avatar */}
          <section className="flex flex-col gap-4 mb-12">
            <h2 className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b" style={{ color: 'var(--c-text-dim)', borderColor: 'var(--c-border)' }}>
              Avatar
            </h2>

            <div className="flex items-center gap-4">
              <div
                data-testid="avatar-preview-container"
                className="w-14 h-14 overflow-hidden flex items-center justify-center flex-shrink-0 cursor-pointer"
                style={{ border: '1px solid var(--c-border)', background: 'var(--c-surface-high)' }}
                onClick={() => fileInputRef.current?.click()}
                title="Click to choose image"
              >
                {preview ? (
                  <img data-testid="avatar-new-preview" src={preview} alt="Preview" className="w-full h-full object-cover" />
                ) : currentAvatarUrl ? (
                  <img
                    data-testid="avatar-current"
                    src={currentAvatarUrl}
                    alt="Avatar"
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
                  Choose image
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
                  aria-label="Select avatar image"
                  className="sr-only"
                />
                <p className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>
                  PNG, JPG, GIF — max 5MB
                </p>
              </div>
            </div>

            {uploadError && (
              <p data-testid="avatar-upload-error" className="text-xs font-mono" style={{ color: '#ff6b6b' }}>
                {uploadError}
              </p>
            )}

            {saveSuccess && !selectedFile && (
              <p data-testid="avatar-upload-success" className="text-xs font-mono" style={{ color: 'var(--c-accent-dim)' }}>
                Avatar updated.
              </p>
            )}

            {selectedFile && (
              <Button
                data-testid="upload-avatar-button"
                onClick={handleAvatarUpload}
                disabled={updateAvatarMutation.isPending}
                isLoading={updateAvatarMutation.isPending}
                loadingText="Uploading…"
              >
                Upload Avatar
              </Button>
            )}
          </section>

          {/* Software Updates */}
          <section className="flex flex-col gap-4 mb-12">
            <h2 className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b" style={{ color: 'var(--c-text-dim)', borderColor: 'var(--c-border)' }}>
              Software Updates
            </h2>

            <div className="flex flex-col gap-2">
              <p className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>
                Current version: <span style={{ color: 'var(--c-text)' }}>{appVersion || "Loading..."}</span>
              </p>

              {updateStatus === "available" && (
                <p className="text-xs font-mono" style={{ color: 'var(--c-accent)' }}>
                  Update available: {updateVersion}
                </p>
              )}

              {updateStatus === "none" && (
                <p className="text-xs font-mono" style={{ color: 'var(--c-accent-dim)' }}>
                  You're up to date!
                </p>
              )}

              {updateStatus === "error" && (
                <p className="text-xs font-mono" style={{ color: '#ff6b6b' }}>
                  {updateError}
                </p>
              )}
            </div>

            <div className="flex gap-2">
              <Button
                onClick={handleCheckForUpdates}
                disabled={updateStatus === "checking"}
                isLoading={updateStatus === "checking"}
                loadingText="Checking…"
              >
                Check for updates
              </Button>

              {updateStatus === "available" && (
                <Button
                  onClick={handleInstallUpdate}
                  disabled={isInstalling}
                  isLoading={isInstalling}
                  loadingText="Installing…"
                  variant="primary"
                >
                  Install update
                </Button>
              )}
            </div>
          </section>

          {/* Danger zone */}
          <section className="flex flex-col gap-4 mb-12" data-testid="settings-danger-zone">
            <h2
              className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
              style={{ color: 'hsl(0 60% 55%)', borderColor: 'hsl(0 60% 30% / 40%)' }}
            >
              Danger Zone
            </h2>

            <p className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>
              Permanently delete your account and all associated data. This cannot be undone.
            </p>

            <TextInput
              label="Type DELETE to confirm"
              id="settings-delete-confirm"
              data-testid="settings-delete-confirm-input"
              value={deleteConfirmText}
              onChange={setDeleteConfirmText}
              placeholder="DELETE"
              disabled={isDeleting}
              error={deleteError || undefined}
            />

            <Button
              data-testid="settings-delete-account-button"
              onClick={handleDeleteAccount}
              disabled={deleteConfirmText !== "DELETE" || isDeleting}
              isLoading={isDeleting}
              loadingText="Deleting account…"
              variant="danger"
              className="w-full"
            >
              Delete my account
            </Button>
          </section>

        </div>
      </div>
    </div>
  );
};
