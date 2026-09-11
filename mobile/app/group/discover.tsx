import { useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Crumb,
  Body,
  Field,
  Button,
  BottomAction,
  Card,
  Ctx,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { useGroupBySlug, useRequestGroupAccess, useMyJoinRequest } from "../../hooks/queries";
import { upper } from "../../i18n";

export default function Discover() {
  const { t } = useTranslation("search");
  const router = useRouter();
  const [slug, setSlug] = useState("");
  const search = useGroupBySlug(slug.trim().replace(/^#/, ""));
  const requestAccess = useRequestGroupAccess();
  const myRequest = useMyJoinRequest(search.data?.id ?? null);

  const onRequest = () => {
    if (!search.data) {
      return;
    }
    requestAccess.mutate(search.data.id);
  };

  const status = myRequest.data?.status;

  return (
    <Screen testID="screen-group-discover">
      <Crumb
        segs={[
          { label: upper(t("nav:breadcrumb.groups")) },
          { label: t("mobile:group.discover.title"), leaf: true },
        ]}
      />
      <Body>
        <View style={{ paddingHorizontal: 18, paddingTop: 12, gap: 8 }}>
          <Text style={ty.label}>{upper(t("group.slugLabel"))}</Text>
          <Field
            amber
            value={slug}
            onChangeText={setSlug}
            placeholder={t("group.slugPlaceholder")}
            testID="input-group-search"
            accessibilityLabel={t("group.slugLabel")}
            icon={<Icon.diamond size={14} color={semantic.mute} />}
          />
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              color: semantic.mute,
              lineHeight: 16,
            }}
          >
            {t("mobile:group.discover.blurb")}
          </Text>
        </View>

        <View style={{ paddingHorizontal: 18, paddingTop: 18 }}>
          {search.isLoading && slug.trim().length >= 2 ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 13,
                color: semantic.mute,
              }}
            >
              {t("group.searching")}
            </Text>
          ) : null}
          {search.isError ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 13,
                color: semantic.danger,
              }}
            >
              {(search.error as Error).message || t("group.notFound")}
            </Text>
          ) : null}
          {search.data ? (
            <Card>
              <Text
                style={{
                  fontFamily: ty.h1.fontFamily,
                  fontSize: 18,
                  color: semantic.ink,
                }}
              >
                {search.data.name}
              </Text>
              {search.data.description ? (
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 13,
                    color: semantic.mute,
                    marginTop: 4,
                  }}
                >
                  {search.data.description}
                </Text>
              ) : null}
              <View style={{ paddingTop: 12 }}>
                {status === "pending" ? (
                  <Text
                    style={[ty.label, { color: semantic.accent }]}
                  >
                    {upper(t("mobile:group.discover.requestPending"))}
                  </Text>
                ) : status === "approved" ? (
                  <Text style={[ty.label, { color: semantic.accent }]}>
                    {upper(t("mobile:group.discover.approved"))}
                  </Text>
                ) : status === "rejected" ? (
                  <Text style={[ty.label, { color: semantic.danger }]}>
                    {upper(t("mobile:group.discover.requestDeclined"))}
                  </Text>
                ) : (
                  <Button
                    full
                    testID="btn-request-access"
                    variant="primary"
                    onPress={onRequest}
                    disabled={requestAccess.isPending}
                    iconRight={<Icon.arrowRight color="#0a0907" />}
                  >
                    {requestAccess.isPending
                      ? upper(t("group.sendingRequest"))
                      : upper(t("group.requestAccess"))}
                  </Button>
                )}
              </View>
            </Card>
          ) : null}
        </View>
      </Body>
      <Ctx
        cr={upper(t("nav:breadcrumb.groups"))}
        name={t("mobile:group.discover.title")}
      />
      <BottomAction>
        <Button
          full
          testID="btn-back"
          variant="subtle"
          onPress={() => router.back()}
          icon={<Icon.back color={semantic.ink} />}
        >
          {t("common:actions.back")}
        </Button>
      </BottomAction>
    </Screen>
  );
}
