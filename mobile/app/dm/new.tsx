import { useState } from "react";
import { View } from "react-native";
import { useOpenConversation } from "../../hooks/useOpenConversation";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Header,
  Body,
  Field,
  ListRow,
  Avatar,
  Button,
  Group,
  Txt,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, space, layout } from "../../theme/tokens";
import { useUserSearch, useCreateDM } from "../../hooks/queries";

export default function NewDM() {
  const { openConversation } = useOpenConversation();
  const { t } = useTranslation("mobile");
  const [query, setQuery] = useState("");
  // Exact-match lookup on Find / return only, never while typing: per-key
  // lookups let anyone walk the directory one prefix at a time (#1216).
  // Desktop's Start DM looks up on submit the same way.
  const [submitted, setSubmitted] = useState("");
  const search = useUserSearch(submitted);
  const submit = () => {
    const q = query.trim();
    if (q.length >= 2) {
      setSubmitted(q);
    }
  };
  const createDM = useCreateDM();

  const onStartDM = (userId: string) => {
    createDM.mutate(
      { memberIds: [userId] },
      {
        onSuccess: (channel) => {
          // Phones: replace this form with the DM. iPad: the DM opens in the
          // Direct tab's two-pane.
          openConversation({ id: channel.id, kind: "dm" }, { replace: true });
        },
      },
    );
  };

  const found = search.data;
  const showEmpty =
    !search.isFetching && !search.isError && submitted.length >= 2 && !found;

  return (
    <Screen testID="screen-dm-new" centered>
      <Header title={t("direct.newMessage")} backTo={t("tabs.direct")} />
      <Body contentContainerStyle={{ padding: space.xxl, gap: space.lg }}>
        <View style={{ gap: space.sm }}>
          <Txt variant="section">
            {t("dm.identifierLabel")}
          </Txt>
          <Field
            value={query}
            onChangeText={setQuery}
            onSubmitEditing={submit}
            returnKeyType="search"
            autoCorrect={false}
            testID="input-user-search"
            accessibilityLabel={t("dm.identifierLabel")}
            placeholder={t("dms:start.identifierPlaceholder")}
            icon={<Icon.search size={18} color={semantic.muted} />}
          />
          <Txt variant="meta">
            {t("dm.exactMatchHint")}
          </Txt>
        </View>
        <Button
          testID="btn-user-search"
          variant="secondary"
          full
          disabled={query.trim().length < 2 || search.isFetching}
          onPress={submit}
        >
          {search.isFetching
            ? t("search:view.searching")
            : t("search:group.submit")}
        </Button>

        {search.isLoading && submitted.length >= 2 ? (
          <Txt variant="secondary">
            {t("search:view.searching")}
          </Txt>
        ) : null}
        {search.isError ? (
          <Txt variant="secondary">
            {t("dm.searchFailed")}
          </Txt>
        ) : null}
        {showEmpty ? (
          <Txt variant="secondary">
            {t("dms:start.userNotFound")}
          </Txt>
        ) : null}
        {found ? (
          <Group>
            <ListRow
              testID={`row-user-${found.id}`}
              accessibilityLabel={t("dm.startWith", { name: found.username })}
              minHeight={64}
              glyph={
                <Avatar
                  label={found.username || found.email || undefined}
                  size={layout.touchMin}
                />
              }
              name={`@${found.username}`}
              sub={found.preferred_name || found.email || undefined}
              onPress={() => onStartDM(found.id)}
              chevron
            />
          </Group>
        ) : null}
        {createDM.isError ? (
          <Txt
            variant="secondary"
            style={{ color: semantic.accent }}
          >
            {(createDM.error as Error).message || t("dms:start.startFailed")}
          </Txt>
        ) : null}
      </Body>
    </Screen>
  );
}
