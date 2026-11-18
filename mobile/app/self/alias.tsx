import { useEffect, useState } from "react";
import { ActivityIndicator, Pressable, View } from "react-native";
import { Image } from "expo-image";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { Text } from "../../components/Text";
import { Screen, Header, Body, Field, Button, BottomAction, Group } from "../../components/ui";
import { UserAvatar } from "../../components/UserAvatar";
import { SettingsField, ErrorText } from "../../components/self/SettingsField";
import { ToggleRow } from "../../components/self/ToggleRow";
import { PhotoPickSheet, uriToPath } from "../../components/self/PhotoPickSheet";
import { Icon } from "../../components/icons";
import { useThemeRerender } from "../../components/theme";
import { semantic, type as ty, space } from "../../theme/tokens";
import { useUserGroupsWithChannels, useUserProfile } from "../../hooks/queries";
import {
  uploadAliasPicture,
  useMyAliases,
  useResetGroupAlias,
  useSetGroupAlias,
} from "../../hooks/queries/useGroupAliases";
import { fieldWrite, isValidAliasName } from "../../lib/groupAlias";
import { appStore } from "../../stores/appStore";

const SIZE = 96;

/**
 * One group's alias (#2003): a name and picture only that group sees. "Apply to all my
 * groups" writes the changed field(s) to every group the user is in now; "Use my main
 * profile" removes the alias here.
 */
function GroupAliasEditor() {
  useThemeRerender();
  const { t } = useTranslation("settings");
  const router = useRouter();
  const { groupId = "" } = useLocalSearchParams<{ groupId?: string }>();
  const userId = appStore.currentUser?.id;
  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === groupId);
  const { data: aliases, isLoading } = useMyAliases();
  const alias = aliases?.get(groupId);
  const { data: profile } = useUserProfile();
  const mainName = profile?.preferred_name || profile?.username || "";
  const setAlias = useSetGroupAlias();
  const reset = useResetGroupAlias();

  const [name, setName] = useState("");
  const [seeded, setSeeded] = useState(false);
  // The picked photo, shown at once while it uploads; `null` key = use the main picture.
  const [preview, setPreview] = useState<string | null>(null);
  const [pickedPath, setPickedPath] = useState<string | null>(null);
  const [clearPicture, setClearPicture] = useState(false);
  const [applyAll, setApplyAll] = useState(false);
  const [sheetOpen, setSheetOpen] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  // Seed once, so a background refetch never clobbers in-progress edits.
  useEffect(() => {
    if (!seeded && !isLoading) {
      setName(alias?.display_name ?? "");
      setSeeded(true);
    }
  }, [alias, isLoading, seeded]);

  const trimmed = name.trim();
  const nameInvalid = !!trimmed && !isValidAliasName(trimmed);
  const busy = setAlias.isPending || reset.isPending || uploading;
  const hasAliasPicture = !!alias?.avatar_url && !clearPicture;

  const onSave = async () => {
    if (nameInvalid || busy) {
      return;
    }
    setError(null);
    setSaved(false);
    // Here: only what changed. Everywhere: the field the user filled in or changed.
    const nameChanged = fieldWrite(trimmed || null, alias?.display_name);
    const displayName = applyAll
      ? trimmed || nameChanged !== undefined
        ? trimmed
        : undefined
      : nameChanged;
    let avatarUrl: string | undefined;
    try {
      if (pickedPath) {
        setUploading(true);
        avatarUrl = await uploadAliasPicture(pickedPath);
      } else if (clearPicture && (applyAll || alias?.avatar_url)) {
        avatarUrl = "";
      }
      if (displayName !== undefined || avatarUrl !== undefined) {
        await setAlias.mutateAsync({ groupId: applyAll ? null : groupId, displayName, avatarUrl });
      }
      setPickedPath(null);
      setPreview(null);
      setClearPicture(false);
      setSaved(true);
    } catch {
      setError(t("aliases.saveFailed"));
    } finally {
      setUploading(false);
    }
  };

  const onUseMain = () => {
    if (!userId) {
      return;
    }
    setError(null);
    reset.mutate(
      { groupId, userId },
      {
        onSuccess: () => {
          setName("");
          setPickedPath(null);
          setPreview(null);
          setClearPicture(false);
          setSaved(true);
        },
        onError: () => setError(t("aliases.saveFailed")),
      },
    );
  };

  return (
    <Screen testID="screen-self-alias">
      <Header title={t("aliases.pageTitle", { group: group?.name ?? "" })} backTo={t("aliases.heading")} />
      <Body contentContainerStyle={{ paddingHorizontal: space.xxl, paddingTop: space.xxl, gap: space.xxl }}>
        <Text style={[ty.secondary, { color: semantic.muted }]}>{t("aliases.pageIntro")}</Text>

        <View style={{ alignItems: "center", gap: space.lg }}>
          <Pressable
            testID="btn-alias-photo"
            accessibilityRole="button"
            accessibilityLabel={t("aliases.choosePicture")}
            accessibilityState={{ disabled: busy, busy }}
            disabled={busy}
            onPress={() => setSheetOpen(true)}
          >
            {preview ? (
              <View
                testID="avatar-alias-preview"
                style={{ width: SIZE, height: SIZE, borderRadius: SIZE / 2, overflow: "hidden", backgroundColor: semantic.high }}
              >
                <Image source={{ uri: preview }} style={{ width: SIZE, height: SIZE }} contentFit="cover" transition={0} />
                {uploading ? (
                  <View
                    style={{
                      position: "absolute",
                      top: 0,
                      bottom: 0,
                      start: 0,
                      end: 0,
                      alignItems: "center",
                      justifyContent: "center",
                      backgroundColor: semantic.backdrop,
                    }}
                  >
                    <ActivityIndicator color={semantic.text} />
                  </View>
                ) : null}
              </View>
            ) : (
              <UserAvatar
                testID="avatar-alias-editor"
                userId={userId}
                avatarKey={profile ? profile.avatar_url ?? null : undefined}
                aliasKey={clearPicture ? null : alias?.avatar_url}
                label={trimmed || mainName}
                size={SIZE}
                variant="self"
              />
            )}
          </Pressable>
          <Button
            testID="btn-alias-photo-change"
            variant="subtle"
            disabled={busy}
            icon={<Icon.camera size={18} color={semantic.text} />}
            onPress={() => setSheetOpen(true)}
          >
            {t("aliases.choosePicture")}
          </Button>
        </View>

        <SettingsField
          label={t("aliases.nameLabel")}
          error={nameInvalid ? t("aliases.nameInvalid") : null}
          errorTestID="text-alias-name-invalid"
        >
          <Field
            value={name}
            onChangeText={(v) => {
              setName(v);
              setSaved(false);
            }}
            placeholder={t("aliases.namePlaceholder", { name: mainName })}
            testID="input-alias-name"
            accessibilityLabel={t("aliases.nameLabel")}
            editable={!busy}
          />
        </SettingsField>

        <Group>
          <ToggleRow
            testID="toggle-alias-apply-all"
            label={t("aliases.applyAll")}
            sub={t("aliases.applyAllHint")}
            on={applyAll}
            onToggle={() => setApplyAll((v) => !v)}
            disabled={busy}
          />
        </Group>

        {error ? <ErrorText>{error}</ErrorText> : null}
        {saved && !busy ? (
          <View style={{ flexDirection: "row", alignItems: "center", gap: space.xs }}>
            <Icon.check size={16} color={semantic.accent} />
            <Text testID="text-alias-saved" accessibilityRole="alert" style={[ty.secondary, { color: semantic.text }]}>
              {t("aliases.saved")}
            </Text>
          </View>
        ) : null}
      </Body>
      <BottomAction>
        <Button
          full
          testID="btn-alias-save"
          variant="primary"
          onPress={() => void onSave()}
          disabled={busy || nameInvalid}
          loading={setAlias.isPending || uploading}
          iconRight={<Icon.check size={18} color={semantic.onAccent} />}
        >
          {setAlias.isPending || uploading ? t("aliases.saving") : t("aliases.save")}
        </Button>
        {alias ? (
          <Button
            variant="subtle"
            full
            testID="btn-alias-use-main"
            onPress={onUseMain}
            disabled={busy}
            loading={reset.isPending}
          >
            {reset.isPending ? t("aliases.resetting") : t("aliases.useMain")}
          </Button>
        ) : null}
      </BottomAction>

      {sheetOpen ? (
        <PhotoPickSheet
          title={t("aliases.pictureLabel")}
          onClose={() => setSheetOpen(false)}
          onPicked={(uri) => {
            setPreview(uri);
            setPickedPath(uriToPath(uri));
            setClearPicture(false);
            setSaved(false);
          }}
          onCameraDenied={() => setError(t("mobile:self.userSettings.photo.cameraDenied"))}
          remove={
            hasAliasPicture || pickedPath
              ? {
                  label: t("aliases.mainPicture"),
                  onPress: () => {
                    setPreview(null);
                    setPickedPath(null);
                    setClearPicture(true);
                    setSaved(false);
                  },
                }
              : undefined
          }
        />
      ) : null}
    </Screen>
  );
}

export default observer(GroupAliasEditor);
