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
  // Looked up on Search / return, never per keystroke: slug lookups share a
  // tight per-IP budget on the DS (60 per 10 min), and a slug typed one
  // character at a time spent ~one lookup per character (#1216). Desktop's
  // Find Group works the same way.
  const [submitted, setSubmitted] = useState("");
  const search = useGroupBySlug(submitted || null);
  const submit = () => {
    const s = slug.trim().replace(/^#/, "");
    if (s.length >= 2) {
      setSubmitted(s);
    }
  };
  const requestAccess = useRequestGroupAccess();
  const myRequest = useMyJoinRequest(search.data?.id ?? null);

  const onRequest = () => {
    if (!search.data) {
      return;
    }
    requestAccess.mutate(search.data.id);
  };

  // The DS never tells a non-member about their own pending request (that
  // would answer "has X asked to join Y" for anyone), so a sent request is
  // shown from the request's own outcome, as desktop does; the lookup only
  // ever answers for admins (#1216).
  const sentForThisGroup = requestAccess.isSuccess && requestAccess.data === search.data?.id;
  const status = sentForThisGroup ? "pending" : myRequest.data?.status;

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
            onSubmitEditing={submit}
            returnKeyType="search"
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

        <View style={{ paddingHorizontal: 18, paddingTop: 14 }}>
          <Button
            testID="btn-group-search"
            variant="subtle"
            full
            disabled={slug.trim().replace(/^#/, "").length < 2 || search.isFetching}
            onPress={submit}
          >
            {upper(search.isFetching ? t("group.searching") : t("group.submit"))}
          </Button>
        </View>
        <View style={{ paddingHorizontal: 18, paddingTop: 18 }}>
          {search.isLoading && !!submitted ? (
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
                {requestAccess.isError ? (
                  <Text
                    testID="discover-request-error"
                    style={{ fontFamily: ty.body.fontFamily, fontSize: 12, color: semantic.danger, marginTop: 8 }}
                  >
                    {(requestAccess.error as Error).message || t("group.requestFailed")}
                  </Text>
                ) : null}
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
