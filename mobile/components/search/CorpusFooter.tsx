import { Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { type as ty, space } from "../../theme/tokens";
import { activeLocale } from "../../i18n";
import type { SearchCorpus } from "../../hooks/queries/useSearch";

/** What this device can search, always shown: the server cannot search for you. */
export function CorpusFooter({ corpus }: { corpus: SearchCorpus }) {
  const { t } = useTranslation("search");
  const earliest = corpus.earliest_sent_at
    ? new Date(corpus.earliest_sent_at).toLocaleDateString(activeLocale())
    : null;
  const style = ty.meta;
  return (
    <View
      testID="search-corpus-footer"
      style={{ paddingVertical: space.xl, gap: 4 }}
    >
      <Text style={style}>
        {earliest
          ? t("view.corpusWithDate", {
              count: corpus.message_count,
              date: earliest,
            })
          : t("view.corpus", { count: corpus.message_count })}
      </Text>
      {corpus.indexing ? (
        <Text testID="search-indexing" style={style}>
          {t("view.indexing")}
        </Text>
      ) : null}
    </View>
  );
}
