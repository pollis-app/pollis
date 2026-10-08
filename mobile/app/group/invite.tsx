import { useState } from "react";
import { View, Text } from "react-native";
import { useNav, useRouteParams } from "../../components/pane/paneContext";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import {
  Screen,
  Header,
  Body,
  Field,
  Button,
  Chip,
  SectionTitle,
  ListRow,
  Group,
} from "../../components/ui";
import { LabeledField, Hint, ErrorText } from "../../components/groups/FormBits";
import { Icon } from "../../components/icons";
import { semantic, type as ty, space } from "../../theme/tokens";
import { CreatedInviteLinkCard } from "../../components/CreatedInviteLinkCard";
import {
  useSendGroupInvite,
  useUserGroupsWithChannels,
  useCreateGroupInviteLink,
  type CreatedInviteLink,
} from "../../hooks/queries";

// Expiry presets, mirroring desktop's InviteLinkManager (#847). A fixed list
// rather than a date picker on purpose — every preset here is one a person
// actually asks for.
const EXPIRY_OPTIONS: {
  id: string;
  label: (t: TFunction) => string;
  hours: number | null;
}[] = [
  {
    id: "24h",
    label: (t) => t("channels:inviteLinks.expiryHours", { count: 24 }),
    hours: 24,
  },
  {
    id: "7d",
    label: (t) => t("channels:inviteLinks.expiryDays", { count: 7 }),
    hours: 24 * 7,
  },
  {
    id: "30d",
    label: (t) => t("channels:inviteLinks.expiryDays", { count: 30 }),
    hours: 24 * 30,
  },
  { id: "never", label: (t) => t("channels:inviteLinks.expiryNever"), hours: null },
];

const USES_OPTIONS: {
  id: string;
  label: (t: TFunction) => string;
  uses: number | null;
}[] = [
  { id: "1", label: (t) => t("channels:inviteLinks.usesOption", { count: 1 }), uses: 1 },
  { id: "10", label: (t) => t("channels:inviteLinks.usesOption", { count: 10 }), uses: 10 },
  { id: "unlimited", label: (t) => t("channels:inviteLinks.usesUnlimited"), uses: null },
];

export default function InviteToGroup() {
  const { t } = useTranslation("channels");
  const router = useNav();
  const { groupId } = useRouteParams<{ groupId?: string }>();
  const [identifier, setIdentifier] = useState("");
  const sendInvite = useSendGroupInvite(groupId ?? null);
  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === groupId);

  // ── #847 shareable link mint state ──────────────────────────────────
  const [expiryHours, setExpiryHours] = useState<number | null>(24 * 7);
  const [maxUses, setMaxUses] = useState<number | null>(null);
  const [created, setCreated] = useState<CreatedInviteLink | null>(null);
  const createLink = useCreateGroupInviteLink(groupId ?? null);

  const onSend = () => {
    const trimmed = identifier.trim();
    if (!trimmed) {
      return;
    }
    sendInvite.mutate(trimmed, {
      onSuccess: () => {
        setIdentifier("");
        router.back();
      },
    });
  };

  const onCreateLink = () => {
    createLink.mutate(
      { expiresInHours: expiryHours, maxUses },
      {
        onSuccess: (link) => setCreated(link),
      },
    );
  };

  return (
    <Screen testID="screen-group-invite" aboveTabBar={router.inPane} centered>
      <Header onBack={router.onBack} title={t("mobile:group.panel.invite")} subtitle={group?.name} />
      <Body contentContainerStyle={{ paddingHorizontal: space.xxl }}>
        <View style={{ paddingTop: space.xxl, gap: space.lg }}>
          <LabeledField
            label={t("mobile:group.invite.identifierLabel")}
            hint={t("mobile:group.invite.blurb", {
              name: group?.name ?? t("mobile:group.invite.thisGroup"),
            })}
          >
            <Field
              value={identifier}
              onChangeText={setIdentifier}
              onSubmitEditing={onSend}
              returnKeyType="send"
              autoCorrect={false}
              testID="input-user-search"
              accessibilityLabel={t("mobile:group.invite.identifierLabel")}
              icon={<Icon.at size={18} color={semantic.muted} />}
            />
          </LabeledField>
          {sendInvite.isError ? (
            <ErrorText>
              {(sendInvite.error as Error).message || t("inviteMember.sendFailed")}
            </ErrorText>
          ) : null}
          {sendInvite.isSuccess ? (
            <Text accessibilityLiveRegion="polite" style={[ty.secondary, { color: semantic.text }]}>
              {t("inviteMember.sent")}
            </Text>
          ) : null}
          <Button
            full
            testID="btn-send-invite"
            variant="primary"
            onPress={onSend}
            disabled={!identifier.trim() || sendInvite.isPending}
          >
            {sendInvite.isPending
              ? t("inviteMember.submitting")
              : t("mobile:group.invite.submit")}
          </Button>
        </View>

        <SectionTitle style={{ paddingHorizontal: 0, paddingTop: space.xxxl * 1.5 }}>
          {t("mobile:group.invite.shareableLink")}
        </SectionTitle>
        <View style={{ gap: space.lg }}>
          <Hint>{t("mobile:group.invite.linkBlurb")}</Hint>

          <Text style={ty.section}>{t("inviteLinks.expiresAfter")}</Text>
          <View style={{ flexDirection: "row", flexWrap: "wrap", gap: space.xs }}>
            {EXPIRY_OPTIONS.map((opt) => (
              <Chip
                key={opt.id}
                variant="outline"
                selected={expiryHours === opt.hours}
                testID={`chip-expiry-${opt.id}`}
                onPress={() => setExpiryHours(opt.hours)}
              >
                {opt.label(t)}
              </Chip>
            ))}
          </View>

          <Text style={ty.section}>{t("inviteLinks.maximumUses")}</Text>
          <View style={{ flexDirection: "row", flexWrap: "wrap", gap: space.xs }}>
            {USES_OPTIONS.map((opt) => (
              <Chip
                key={opt.id}
                variant="outline"
                selected={maxUses === opt.uses}
                testID={`chip-uses-${opt.id}`}
                onPress={() => setMaxUses(opt.uses)}
              >
                {opt.label(t)}
              </Chip>
            ))}
          </View>

          <Button
            full
            testID="btn-create-invite-link"
            onPress={onCreateLink}
            disabled={createLink.isPending}
            icon={<Icon.link size={18} color={semantic.text} />}
          >
            {createLink.isPending ? t("inviteLinks.creating") : t("inviteLinks.create")}
          </Button>

          {createLink.isError ? (
            <ErrorText>
              {(createLink.error as Error).message ||
                t("mobile:group.invite.createLinkFailed")}
            </ErrorText>
          ) : null}

          {created ? <CreatedInviteLinkCard link={created} /> : null}

          <Group>
            <ListRow
              testID="row-manage-invite-links"
              glyph={<Icon.link size={20} color={semantic.dim} />}
              name={t("mobile:group.invite.manageLinks")}
              sub={t("mobile:group.invite.manageLinksSub")}
              chevron
              onPress={() =>
                groupId &&
                router.push({
                  pathname: "/group/invite-links",
                  params: { groupId },
                })
              }
            />
          </Group>
        </View>
      </Body>
    </Screen>
  );
}
