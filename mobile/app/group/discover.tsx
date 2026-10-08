import { useState } from "react";
import { View, Text } from "react-native";
import { useTranslation } from "react-i18next";
import { Screen, Header, Body, Field, Button, Card } from "../../components/ui";
import { LabeledField, Hint, ErrorText } from "../../components/groups/FormBits";
import { Icon } from "../../components/icons";
import { semantic, type as ty, fonts, space } from "../../theme/tokens";
import { useGroupBySlug, useRequestGroupAccess, useMyJoinRequest } from "../../hooks/queries";

export default function Discover() {
  const { t } = useTranslation("search");
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

  const statusText =
    status === "pending"
      ? t("mobile:group.discover.requestPending")
      : status === "approved"
        ? t("mobile:group.discover.approved")
        : status === "rejected"
          ? t("mobile:group.discover.requestDeclined")
          : null;

  return (
    <Screen testID="screen-group-discover">
      <Header title={t("mobile:group.discover.title")} />
      <Body contentContainerStyle={{ padding: space.xxl, gap: space.xxl }}>
        <LabeledField label={t("mobile:group.discover.slugLabel")} hint={t("mobile:group.discover.blurb")}>
          <Field
            value={slug}
            onChangeText={setSlug}
            onSubmitEditing={submit}
            returnKeyType="search"
            placeholder={t("group.slugPlaceholder")}
            testID="input-group-search"
            accessibilityLabel={t("mobile:group.discover.slugLabel")}
            icon={<Icon.search size={18} color={semantic.muted} />}
          />
        </LabeledField>
        <Button
          testID="btn-group-search"
          full
          disabled={slug.trim().replace(/^#/, "").length < 2 || search.isFetching}
          onPress={submit}
        >
          {search.isFetching ? t("group.searching") : t("group.submit")}
        </Button>

        {search.isLoading && !!submitted ? <Hint>{t("group.searching")}</Hint> : null}
        {search.isError ? (
          <ErrorText>{(search.error as Error).message || t("group.notFound")}</ErrorText>
        ) : null}
        {search.data ? (
          <Card style={{ gap: space.md }}>
            <View style={{ gap: 4 }}>
              <Text accessibilityRole="header" style={ty.heading}>
                {search.data.name}
              </Text>
              {search.data.description ? (
                <Text style={ty.secondary}>{search.data.description}</Text>
              ) : null}
            </View>
            {statusText ? (
              <View
                accessible
                accessibilityLiveRegion="polite"
                style={{ flexDirection: "row", alignItems: "center", gap: space.sm }}
              >
                {status === "rejected" ? (
                  <Icon.alert size={16} color={semantic.dim} />
                ) : (
                  <Icon.check size={16} color={semantic.accent} />
                )}
                <Text style={{ flex: 1, fontFamily: fonts.semibold, fontSize: 15, color: semantic.text }}>
                  {statusText}
                </Text>
              </View>
            ) : (
              <Button
                full
                testID="btn-request-access"
                surface="raised"
                variant="primary"
                onPress={onRequest}
                disabled={requestAccess.isPending}
              >
                {requestAccess.isPending
                  ? t("group.sendingRequest")
                  : t("mobile:group.discover.requestAccess")}
              </Button>
            )}
            {requestAccess.isError ? (
              <ErrorText testID="discover-request-error">
                {(requestAccess.error as Error).message || t("group.requestFailed")}
              </ErrorText>
            ) : null}
          </Card>
        ) : null}
      </Body>
    </Screen>
  );
}
