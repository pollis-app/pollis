import { useState } from "react";
import { Text } from "react-native";
import { useTranslation } from "react-i18next";
import { SheetOverlay, afterSheetClose } from "../chat/SheetOverlay";
import { Button, Field } from "../ui";
import { Icon } from "../icons";
import { ErrorText } from "./FormBits";
import { semantic, type as ty } from "../../theme/tokens";
import { useCreateChannel } from "../../hooks/queries";
import type { Channel } from "../../types";

// "+" next to "Text channels": name a new text channel (admins only, as on
// desktop's group menu). Voice is not offered — mobile has no voice.
export function CreateChannelSheet({
  groupId,
  existingNames,
  onClose,
  onCreated,
}: {
  groupId: string;
  // Lower-cased names already in the group, to refuse a duplicate up front.
  existingNames: string[];
  onClose: () => void;
  onCreated: (channel: Channel) => void;
}) {
  const { t } = useTranslation("mobile");
  const [name, setName] = useState("");
  const createChannel = useCreateChannel(groupId);
  const trimmed = name.trim();
  const duplicate = existingNames.includes(trimmed.toLowerCase());

  const onSubmit = () => {
    if (!trimmed || duplicate) {
      return;
    }
    createChannel.mutate(
      { name: trimmed },
      {
        onSuccess: (channel) => {
          onClose();
          afterSheetClose(() => onCreated(channel));
        },
      },
    );
  };

  return (
    <SheetOverlay title={t("group.newChannel.title")} onClose={onClose} testID="sheet-create-channel">
      <Text style={ty.section}>{t("group.newChannel.nameLabel")}</Text>
      <Field
        testID="input-channel-name"
        accessibilityLabel={t("group.newChannel.nameLabel")}
        value={name}
        onChangeText={setName}
        onSubmitEditing={onSubmit}
        returnKeyType="done"
        autoFocus
        placeholder={t("channels:createChannel.namePlaceholder")}
        icon={<Icon.hash size={16} color={semantic.muted} />}
      />
      {duplicate ? (
        <ErrorText>
          {t("channels:createChannel.duplicate", { slug: trimmed })}
        </ErrorText>
      ) : null}
      {createChannel.isError ? (
        <ErrorText>
          {(createChannel.error as Error).message || t("channels:createChannel.createFailed")}
        </ErrorText>
      ) : null}
      <Button
        full
        variant="primary"
        testID="btn-create-channel-submit"
        onPress={onSubmit}
        disabled={!trimmed || duplicate || createChannel.isPending}
      >
        {createChannel.isPending
          ? t("channels:createChannel.submitting")
          : t("group.newChannel.submit")}
      </Button>
    </SheetOverlay>
  );
}
