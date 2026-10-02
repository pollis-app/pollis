import { Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { semantic, type as ty } from "../../theme/tokens";
import { activeLocale } from "../../i18n";
import type { SearchCorpus } from "../../hooks/queries/useSearch";

/**
 * What this device can search. Persistent rather than shown only on "no
 * results", because E2EE means the server cannot search for you — the limits
 * of the on-device corpus are the answer to most "why isn't it here" questions.
 * Same copy as desktop's CorpusFooter (shared `search:view.*` strings).
 */
export function CorpusFooter({ corpus }: { corpus: SearchCorpus }) {
  const { t } = useTranslation("search");
  const earliest = corpus.earliest_sent_at
    ? new Date(corpus.earliest_sent_at).toLocaleDateString(activeLocale())
    : null;
  const style = { fontFamily: ty.body.fontFamily, fontSize: 11, color: semantic.mute };
  return (
    <View testID="search-corpus-footer" style={{ paddingHorizontal: 18, paddingVertical: 14, gap: 4 }}>
      <Text style={style}>
        {earliest
          ? t("view.corpusWithDate", { count: corpus.message_count, date: earliest })
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
