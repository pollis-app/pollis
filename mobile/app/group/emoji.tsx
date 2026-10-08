import { useMemo, useState } from "react";
import { View } from "react-native";
import { useNav, useRouteParams } from "../../components/pane/paneContext";
import { useTranslation } from "react-i18next";
import * as ImagePicker from "expo-image-picker";
import {
  Screen,
  Header,
  Body,
  SectionTitle,
  ListRow,
  Group,
  Field,
  Button,
  Chip,
} from "../../components/ui";
import { LabeledField, Hint, ErrorText } from "../../components/groups/FormBits";
import { Icon } from "../../components/icons";
import { semantic, space } from "../../theme/tokens";
import {
  useUserGroupsWithChannels,
  useGroupMembers,
  useGroupEmoji,
  useUsableEmoji,
  useUploadGroupEmoji,
  useRemoveGroupEmoji,
  SHORTCODE_RE,
} from "../../hooks/queries";
import { CustomEmojiImage } from "../../components/emoji/CustomEmojiImage";
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
  const { groupId } = useRouteParams<{ groupId?: string }>();
  const router = useNav();
  const id = groupId ?? null;
  const currentUser = appStore.currentUser;

  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === id);
  const { data: members = [] } = useGroupMembers(id);
  // The group's own list is a separate DS read that can lag far behind on a
  // busy bridge (a just-created group is still setting up MLS), which left
  // this page on "Loading…". The usable set the composer already fetched
  // holds every emoji of every group the user is in, so its slice for this
  // group stands in until the group's own list arrives.
  const groupEmoji = useGroupEmoji(id);
  const usable = useUsableEmoji();
  const fromUsable = useMemo(
    () => (usable.data ? usable.data.filter((e) => e.group_id === id) : undefined),
    [usable.data, id],
  );
  const emoji = groupEmoji.data ?? fromUsable ?? [];
  const isLoading = groupEmoji.isLoading && fromUsable === undefined;
  const loadFailed = groupEmoji.isError && fromUsable === undefined;
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
    // The button stays enabled (a disabled secondary reads as plain text);
    // a missing or bad shortcode is explained instead.
    if (!shortcode) {
      setPickError(t("manage.shortcodeRequired"));
      return;
    }
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
    <Screen testID="screen-group-emoji" aboveTabBar={router.inPane}>
      <Header onBack={router.onBack} title={t("mobile:group.panel.customEmoji")} subtitle={group ? groupName : undefined} />
      <Body contentContainerStyle={{ paddingHorizontal: space.xxl }}>
        <SectionTitle style={{ paddingHorizontal: 0 }}>
          {t("mobile:group.panel.customEmoji")}
        </SectionTitle>
        {isLoading ? <Hint>{t("common:states.loading")}</Hint> : null}
        {loadFailed ? <ErrorText>{t("mobile:group.emoji.loadFailed")}</ErrorText> : null}
        {!isLoading && !loadFailed && emoji.length === 0 ? (
          <Hint>{t("mobile:group.emoji.empty")}</Hint>
        ) : null}
        {emoji.length > 0 ? (
          <Group>
            {emoji.map((e) => {
              const armed = confirmRemove === e.shortcode;
              const size = t("mobile:group.emoji.sizeKb", {
                count: Math.max(1, Math.round(e.size_bytes / 1024)),
              });
              return (
                <ListRow
                  key={e.shortcode}
                  testID={`row-emoji-${e.shortcode}`}
                  glyph={
                    <CustomEmojiImage
                      shortcode={e.shortcode}
                      contentHash={e.content_hash}
                      size={22}
                    />
                  }
                  name={`:${e.shortcode}:`}
                  sub={e.animated ? t("manage.sizeAnimated", { size }) : size}
                  end={
                    iAmAdmin ? (
                      <Chip
                        variant="outline"
                        selected={armed}
                        testID={`btn-remove-emoji-${e.shortcode}`}
                        accessibilityLabel={
                          armed
                            ? t("mobile:group.settings.tapAgainToConfirm")
                            : t("manage.remove", { shortcode: e.shortcode })
                        }
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
          </Group>
        ) : null}
        {remove.isError ? (
          <View style={{ paddingTop: space.md }}>
            <ErrorText>{(remove.error as Error).message || t("manage.removeFailed")}</ErrorText>
          </View>
        ) : null}

        {iAmAdmin ? (
          <View>
            <SectionTitle style={{ paddingHorizontal: 0, paddingBottom: space.xxl }}>{t("manage.add")}</SectionTitle>
            <View style={{ gap: space.lg }}>
              <LabeledField
                label={t("manage.shortcodeLabel")}
                error={
                  shortcode.length > 0 && !shortcodeValid
                    ? t("manage.shortcodeInvalid")
                    : shortcodeTaken
                      ? t("manage.shortcodeTaken", { shortcode })
                      : null
                }
              >
                <Field
                  value={shortcode}
                  onChangeText={(v) => setShortcode(v.toLowerCase())}
                  placeholder={t("manage.shortcodePlaceholder")}
                  autoCorrect={false}
                  testID="input-emoji-shortcode"
                  accessibilityLabel={t("manage.shortcodeLabel")}
                />
              </LabeledField>
              <Button
                full
                testID="btn-upload-emoji"
                variant="secondary"
                icon={<Icon.plus size={18} color={semantic.accent} />}
                onPress={() => void onPickAndUpload()}
                disabled={upload.isPending}
              >
                {upload.isPending
                  ? t("mobile:group.emoji.uploading")
                  : t("mobile:group.emoji.pickAndUpload")}
              </Button>
              {upload.isError ? (
                <ErrorText>{(upload.error as Error).message || t("manage.addFailed")}</ErrorText>
              ) : null}
              {pickError ? <ErrorText>{pickError}</ErrorText> : null}
            </View>
          </View>
        ) : (
          <View style={{ paddingTop: space.xxl }}>
            <Hint>{t("mobile:group.emoji.adminsOnly")}</Hint>
          </View>
        )}
      </Body>
    </Screen>
  );
}

export default observer(GroupEmoji);
