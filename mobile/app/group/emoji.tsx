import { useState } from "react";
import { View, Text } from "react-native";
import { useLocalSearchParams } from "expo-router";
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

  return (
    <Screen testID="screen-group-emoji">
      <Crumb
        segs={[
          { label: "GROUPS" },
          { label: group?.name ?? "Group" },
          { label: "Emoji", leaf: true },
        ]}
      />
      <Body>
        <SectionTitle>CUSTOM EMOJI</SectionTitle>
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
            Loading…
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
            No custom emoji yet.
          </Text>
        ) : null}
        {emoji.map((e) => {
          const armed = confirmRemove === e.shortcode;
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
              sub={`${Math.max(1, Math.round(e.size_bytes / 1024))} KB${e.animated ? " · animated" : ""}`}
              end={
                iAmAdmin ? (
                  <Chip
                    variant={armed ? "on" : "default"}
                    testID={`btn-remove-emoji-${e.shortcode}`}
                    accessibilityLabel={`Remove :${e.shortcode}:`}
                    onPress={() => onRemove(e.shortcode)}
                  >
                    {remove.isPending && armed
                      ? "…"
                      : armed
                        ? "Confirm"
                        : "Remove"}
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
            {(remove.error as Error).message || "Couldn't remove emoji."}
          </Text>
        ) : null}

        {iAmAdmin ? (
          <View>
            <SectionTitle>ADD EMOJI</SectionTitle>
            <View style={{ paddingHorizontal: 18, paddingTop: 6, gap: 6 }}>
              <Text style={ty.label}>SHORTCODE</Text>
              <Field
                value={shortcode}
                onChangeText={(v) => setShortcode(v.toLowerCase())}
                placeholder="party_parrot"
                testID="input-emoji-shortcode"
                accessibilityLabel="Emoji shortcode"
              />
              {shortcode.length > 0 && !shortcodeValid ? (
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 12,
                    color: semantic.danger,
                  }}
                >
                  2–32 characters: a–z, 0–9, underscore.
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
                  That shortcode is already taken in this group.
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
                  {upload.isPending ? "UPLOADING…" : "PICK IMAGE & UPLOAD"}
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
                  {(upload.error as Error).message || "Couldn't upload emoji."}
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
            Only group admins can add or remove emoji.
          </Text>
        )}
      </Body>
      <Ctx cr="GROUP" name={`${group?.name ?? "Group"} emoji`} />
    </Screen>
  );
}

export default observer(GroupEmoji);
