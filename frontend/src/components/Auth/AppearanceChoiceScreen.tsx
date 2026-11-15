import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { TitleBar } from "../Layout/TitleBar";
import { Card } from "../ui/Card";
import { Button } from "../ui/Button";
import { Avatar } from "../ui/Avatar";
import { SkinPicker } from "../Preferences/SkinPicker";
import { usePreferences } from "../../hooks/queries/usePreferences";
import { applySkin, type Skin } from "../../utils/colorUtils";
import { DEFAULT_SKIN } from "../../utils/firstRunAppearance";

interface AppearanceChoiceScreenProps {
  onDone: () => void;
}

/** Two sample lines drawn the way the chosen skin draws a conversation. */
const SkinPreview: React.FC<{ skin: Skin }> = ({ skin }) => {
  const { t } = useTranslation("settings");
  const lines = [
    { time: "10:02", name: "ada", text: t("appearance.firstRun.sampleMessage") },
    { time: "10:03", name: "sam", text: t("appearance.firstRun.sampleReply") },
  ];
  return (
    <div
      data-testid="appearance-choice-preview"
      data-skin-preview={skin}
      aria-label={t("appearance.firstRun.preview")}
      role="img"
      className="rounded-panel bg-bg px-4 py-3 flex flex-col gap-2"
    >
      {lines.map((line) =>
        skin === DEFAULT_SKIN ? (
          <p key={line.name} className="font-machine text-xs text-fg truncate">
            <span className="text-muted">{line.time} </span>
            <span className="text-accent">&lt;{line.name}&gt;</span> {line.text}
          </p>
        ) : (
          <div key={line.name} className="flex items-start gap-3">
            <Avatar size={28} />
            <div className="min-w-0 flex flex-col">
              <span className="text-sm text-fg">
                <span className="font-semibold">{line.name}</span>
                <span className="font-machine text-xs text-muted ms-2">{line.time}</span>
              </span>
              <span className="text-sm text-dim truncate">{line.text}</span>
            </div>
          </div>
        ),
      )}
    </div>
  );
};

/**
 * #1924: a new user's first sign-in picks Terminal or Modern before the app opens. A full-window
 * step, not a dialog; the pick re-skins the window live, and Continue saves it to the synced
 * preferences, so the account is never asked again.
 */
export const AppearanceChoiceScreen: React.FC<AppearanceChoiceScreenProps> = ({ onDone }) => {
  const { t } = useTranslation("settings");
  const { query, saveAndWait } = usePreferences();
  const [skin, setSkin] = useState<Skin>(DEFAULT_SKIN);
  const [saving, setSaving] = useState(false);
  const [failed, setFailed] = useState(false);

  // Re-applied when the preferences land, so their load cannot repaint over the pick.
  useEffect(() => {
    applySkin(skin);
  }, [skin, query.dataUpdatedAt]);

  const save = async () => {
    if (saving || !query.data) {
      return;
    }
    setSaving(true);
    setFailed(false);
    try {
      await saveAndWait({ ...query.data, skin });
      onDone();
    } catch {
      setFailed(true);
      setSaving(false);
    }
  };

  return (
    <div data-testid="appearance-choice-screen" className="flex flex-col h-full w-full bg-bg">
      <TitleBar />
      <div className="flex-1 flex justify-center overflow-y-auto px-4">
        <Card padding="lg" className="my-auto w-full max-w-md">
          <div className="flex flex-col gap-6">
            <div className="flex flex-col gap-2">
              <h2 className="text-lg text-fg" data-testid="appearance-choice-title">
                {t("appearance.firstRun.title")}
              </h2>
              <p className="text-sm text-muted leading-relaxed">{t("appearance.firstRun.subtitle")}</p>
            </div>
            <SkinPicker data-testid="first-run-skin" value={skin} onChange={setSkin} />
            <SkinPreview skin={skin} />
            {failed && (
              <p className="text-sm text-danger" role="alert" data-testid="appearance-choice-error">
                {t("appearance.firstRun.saveFailed")}
              </p>
            )}
            <div>
              <Button
                data-testid="appearance-choice-continue"
                isLoading={saving}
                disabled={!query.data}
                onClick={save}
              >
                {t("appearance.firstRun.continue")}
              </Button>
            </div>
          </div>
        </Card>
      </div>
    </div>
  );
};
