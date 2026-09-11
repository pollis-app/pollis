import { useState } from "react";
import { View, Text } from "react-native";
import { useLocalSearchParams } from "expo-router";
import { useTranslation } from "react-i18next";
import * as ImagePicker from "expo-image-picker";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Field,
  Button,
  Chip,
  Ctx,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import {
  useUserGroupsWithChannels,
  useGroupMembers,
  useGroupEmoji,
  useUploadGroupEmoji,
  useRemoveGroupEmoji,
  SHORTCODE_RE,
} from "../../hooks/queries";
import { CustomEmojiImage } from "../../components/emoji/CustomEmojiImage";
import { upper } from "../../i18n";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

/** `file://` URI → bare filesystem path for the Rust side. */
function uriToPath(uri: string): string {
  if (uri.startsWith("file://")) {
    return decodeURIComponent(uri.slice("file://".length));
  }
  return uri;
}

function GroupEmoji() {
  const { t } = useTranslation("emoji");
  const { groupId } = useLocalSearchParams<{ groupId?: string }>();
  const id = groupId ?? null;
  const currentUser = appStore.currentUser;

  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === id);
  const { data: members = [] } = useGroupMembers(id);
  const { data: emoji = [], isLoading } = useGroupEmoji(id);
  const upload = useUploadGroupEmoji(id);
  const remove = useRemoveGroupEmoji(id);

  const myRole = members.find((m) => m.user_id === currentUser?.id)?.role;
  const iAmAdmin = myRole === "admin" || myRole === "owner";

  const [shortcode, setShortcode] = useState("");
  const [pickError, setPickError] = useState<string | null>(null);
  const [confirmRemove, setConfirmRemove] = useState<string | null>(null);

  const shortcodeValid = SHORTCODE_RE.test(shortcode);
  const shortcodeTaken = emoji.some((e) => e.shortcode === shortcode);

  const onPickAndUpload = async () => {
    setPickError(null);
    if (!shortcodeValid || shortcodeTaken) {
      return;
    }
    const result = await ImagePicker.launchImageLibraryAsync({
      mediaTypes: "images",
      quality: 1,
    });
    if (result.canceled || result.assets.length === 0) {
      return;
    }
    const asset = result.assets[0];
    upload.mutate(
      { shortcode, path: uriToPath(asset.uri) },
      {
        onSuccess: () => setShortcode(""),
      },
    );
  };

  const onRemove = (code: string) => {
    if (confirmRemove !== code) {
      setConfirmRemove(code);
      return;
    }
    remove.mutate(code, {
      onSettled: () => setConfirmRemove(null),
    });
  };

  const groupName = group?.name ?? t("mobile:group.common.fallbackName");

  return (
    <Screen testID="screen-group-emoji">
      <Crumb
        segs={[
          { label: upper(t("nav:breadcrumb.groups")) },
          { label: groupName },
          { label: t("mobile:group.common.emoji"), leaf: true },
        ]}
      />
      <Body>
        <SectionTitle>{upper(t("manage.title"))}</SectionTitle>
        {isLoading ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingTop: 6,
            }}
          >
            {t("common:states.loading")}
          </Text>
        ) : null}
        {!isLoading && emoji.length === 0 ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingTop: 6,
            }}
          >
            {t("mobile:group.emoji.empty")}
          </Text>
        ) : null}
        {emoji.map((e) => {
          const armed = confirmRemove === e.shortcode;
          const size = t("mobile:group.emoji.sizeKb", {
            count: Math.max(1, Math.round(e.size_bytes / 1024)),
          });
          return (
            <ListRow
              key={e.shortcode}
              testID={`row-emoji-${e.shortcode}`}
              minHeight={48}
              glyph={
                <CustomEmojiImage
                  shortcode={e.shortcode}
                  contentHash={e.content_hash}
                  size={22}
                />
              }
              name={`:${e.shortcode}:`}
              nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
              sub={e.animated ? t("manage.sizeAnimated", { size }) : size}
              end={
                iAmAdmin ? (
                  <Chip
                    variant={armed ? "on" : "default"}
                    testID={`btn-remove-emoji-${e.shortcode}`}
                    accessibilityLabel={t("manage.remove", { shortcode: e.shortcode })}
                    onPress={() => onRemove(e.shortcode)}
                  >
                    {remove.isPending && armed
                      ? "…"
                      : armed
                        ? t("mobile:group.common.confirm")
                        : t("mobile:group.common.remove")}
                  </Chip>
                ) : null
              }
            />
          );
        })}
        {remove.isError ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.danger,
              paddingHorizontal: 18,
              paddingTop: 6,
            }}
          >
            {(remove.error as Error).message || t("manage.removeFailed")}
          </Text>
        ) : null}

        {iAmAdmin ? (
          <View>
            <SectionTitle>{upper(t("manage.add"))}</SectionTitle>
            <View style={{ paddingHorizontal: 18, paddingTop: 6, gap: 6 }}>
              <Text style={ty.label}>{upper(t("manage.shortcodeLabel"))}</Text>
              <Field
                value={shortcode}
                onChangeText={(v) => setShortcode(v.toLowerCase())}
                placeholder={t("manage.shortcodePlaceholder")}
                testID="input-emoji-shortcode"
                accessibilityLabel={t("manage.shortcodeLabel")}
              />
              {shortcode.length > 0 && !shortcodeValid ? (
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 12,
                    color: semantic.danger,
                  }}
                >
                  {t("manage.shortcodeInvalid")}
                </Text>
              ) : null}
              {shortcodeTaken ? (
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 12,
                    color: semantic.danger,
                  }}
                >
                  {t("manage.shortcodeTaken", { shortcode })}
                </Text>
              ) : null}
              <View style={{ paddingTop: 8 }}>
                <Button
                  full
                  testID="btn-upload-emoji"
                  icon={<Icon.plus color={semantic.ink} />}
                  onPress={() => void onPickAndUpload()}
                  disabled={
                    !shortcodeValid || shortcodeTaken || upload.isPending
                  }
                >
                  {upload.isPending
                    ? upper(t("mobile:group.emoji.uploading"))
                    : upper(t("mobile:group.emoji.pickAndUpload"))}
                </Button>
              </View>
              {upload.isError ? (
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 12,
                    color: semantic.danger,
                    paddingTop: 6,
                  }}
                >
                  {(upload.error as Error).message || t("manage.addFailed")}
                </Text>
              ) : null}
              {pickError ? (
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 12,
                    color: semantic.danger,
                    paddingTop: 6,
                  }}
                >
                  {pickError}
                </Text>
              ) : null}
            </View>
          </View>
        ) : (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingTop: 14,
            }}
          >
            {t("mobile:group.emoji.adminsOnly")}
          </Text>
        )}
      </Body>
      <Ctx
        cr={upper(t("mobile:group.common.fallbackName"))}
        name={t("mobile:group.emoji.ctxTitle", { name: groupName })}
      />
    </Screen>
  );
}

export default observer(GroupEmoji);
