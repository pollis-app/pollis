import { useState } from "react";
import { View, Text } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import {
  Screen,
  Crumb,
  Body,
  Field,
  Button,
  BottomAction,
  Chip,
  SectionTitle,
  ListRow,
  Ctx,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { CreatedInviteLinkCard } from "../../components/CreatedInviteLinkCard";
import {
  useSendGroupInvite,
  useUserGroupsWithChannels,
  useCreateGroupInviteLink,
  type CreatedInviteLink,
} from "../../hooks/queries";
import { upper } from "../../i18n";

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
  const router = useRouter();
  const { groupId } = useLocalSearchParams<{ groupId?: string }>();
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
    <Screen testID="screen-group-invite" centered>
      <Crumb
        segs={[
          { label: upper(t("nav:breadcrumb.groups")) },
          { label: group?.name ?? t("mobile:group.common.fallbackName") },
          { label: t("mobile:group.invite.crumb"), leaf: true },
        ]}
      />
      <Body>
        <View style={{ paddingHorizontal: 18, paddingTop: 12, gap: 8 }}>
          <Text style={ty.label}>{upper(t("inviteMember.identifierLabel"))}</Text>
          <Field
            amber
            value={identifier}
            onChangeText={setIdentifier}
            testID="input-user-search"
            accessibilityLabel={t("inviteMember.identifierLabel")}
            icon={<Icon.at color={semantic.mute} />}
          />
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              color: semantic.mute,
              lineHeight: 16,
            }}
          >
            {t("mobile:group.invite.blurb", {
              name: group?.name ?? t("mobile:group.invite.thisGroup"),
            })}
          </Text>
          {sendInvite.isError ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.danger,
                paddingTop: 8,
              }}
            >
              {(sendInvite.error as Error).message || t("inviteMember.sendFailed")}
            </Text>
          ) : null}
          {sendInvite.isSuccess ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.accent,
                paddingTop: 8,
              }}
            >
              {t("inviteMember.sent")}
            </Text>
          ) : null}
        </View>

        <SectionTitle>{upper(t("mobile:group.invite.shareableLink"))}</SectionTitle>
        <View style={{ paddingHorizontal: 18, paddingTop: 6, gap: 8 }}>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              color: semantic.mute,
              lineHeight: 16,
            }}
          >
            {t("mobile:group.invite.linkBlurb")}
          </Text>

          <Text style={[ty.label, { paddingTop: 6 }]}>
            {upper(t("inviteLinks.expiresAfter"))}
          </Text>
          <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
            {EXPIRY_OPTIONS.map((opt) => (
              <Chip
                key={opt.id}
                variant={expiryHours === opt.hours ? "on" : "default"}
                testID={`chip-expiry-${opt.id}`}
                onPress={() => setExpiryHours(opt.hours)}
              >
                {upper(opt.label(t))}
              </Chip>
            ))}
          </View>

          <Text style={[ty.label, { paddingTop: 6 }]}>
            {upper(t("inviteLinks.maximumUses"))}
          </Text>
          <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
            {USES_OPTIONS.map((opt) => (
              <Chip
                key={opt.id}
                variant={maxUses === opt.uses ? "on" : "default"}
                testID={`chip-uses-${opt.id}`}
                onPress={() => setMaxUses(opt.uses)}
              >
                {upper(opt.label(t))}
              </Chip>
            ))}
          </View>

          <View style={{ paddingTop: 8 }}>
            <Button
              full
              testID="btn-create-invite-link"
              onPress={onCreateLink}
              disabled={createLink.isPending}
              icon={<Icon.link color={semantic.ink} />}
            >
              {createLink.isPending
                ? upper(t("inviteLinks.creating"))
                : upper(t("inviteLinks.create"))}
            </Button>
          </View>

          {createLink.isError ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.danger,
              }}
            >
              {(createLink.error as Error).message ||
                t("mobile:group.invite.createLinkFailed")}
            </Text>
          ) : null}

          {created ? <CreatedInviteLinkCard link={created} /> : null}
        </View>

        <ListRow
          testID="row-manage-invite-links"
          minHeight={48}
          glyph={<Icon.link color={semantic.mute} />}
          name={t("mobile:group.invite.manageLinks")}
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          sub={t("mobile:group.invite.manageLinksSub")}
          onPress={() =>
            groupId &&
            router.push({
              pathname: "/group/invite-links",
              params: { groupId },
            })
          }
          end={<Icon.fwd color={semantic.mute} />}
        />
      </Body>
      <Ctx
        cr={group?.name ?? upper(t("mobile:group.common.fallbackName"))}
        name={t("group.inviteMember")}
      />
      <BottomAction>
        <Button
          full
          testID="btn-send-invite"
          variant="primary"
          onPress={onSend}
          disabled={!identifier.trim() || sendInvite.isPending}
          iconRight={<Icon.arrowRight color="#0a0907" />}
        >
          {sendInvite.isPending
            ? upper(t("inviteMember.submitting"))
            : upper(t("inviteMember.submit"))}
        </Button>
        <Button
          variant="subtle"
          full
          testID="btn-cancel"
          onPress={() => router.back()}
        >
          {t("common:actions.cancel")}
        </Button>
      </BottomAction>
    </Screen>
  );
}
