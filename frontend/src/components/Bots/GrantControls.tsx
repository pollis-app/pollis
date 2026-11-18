import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { AdminGrants } from "../../types";
import type { GrantNarrowing } from "../../utils/botApps";
import { Checkbox } from "../ui/Checkbox";
import { Switch } from "../ui/Switch";
import { TextInput } from "../ui/TextInput";

interface GrantControlsProps {
  /** The most the approval may grant; every control only takes away from it. */
  ceiling: AdminGrants;
  narrowing: GrantNarrowing;
  onChange: (n: GrantNarrowing) => void;
  disabled?: boolean;
  testId: string;
}

function toggle(list: ReadonlyArray<string>, item: string, drop: boolean): string[] {
  const rest = list.filter((x) => x !== item);
  return drop ? [...rest, item] : rest;
}

/**
 * The admin's narrowing (#2005): deselect channels, shorten the history window, drop hosts it
 * may send data to. Nothing here can grant more than `ceiling`; events and actions stay as given.
 */
export const GrantControls: React.FC<GrantControlsProps> = ({ ceiling, narrowing, onChange, disabled, testId }) => {
  const { t } = useTranslation("channels");
  const history = narrowing.history ?? ceiling.history;
  const [historyText, setHistoryText] = useState(String(history));

  // A new ceiling (a widening turned on or off) can move the shown value.
  useEffect(() => {
    setHistoryText(String(history));
  }, [history]);

  const setHistory = (text: string) => {
    setHistoryText(text);
    const n = Number.parseInt(text, 10);
    if (Number.isNaN(n)) {
      return;
    }
    onChange({ ...narrowing, history: Math.max(0, Math.min(ceiling.history, n)) });
  };

  return (
    <div className="flex flex-col gap-4" data-testid={testId}>
      <div className="flex flex-col gap-1">
        <h3 className="text-heading text-fg">{t("bots.app.grants.heading")}</h3>
        <p className="text-secondary text-muted">{t("bots.app.grants.description")}</p>
      </div>

      <fieldset className="flex flex-col gap-2" disabled={disabled}>
        <legend className="mb-2 text-secondary font-medium text-fg">{t("bots.app.grants.channels")}</legend>
        {ceiling.channels.length === 0 ? (
          <p className="text-secondary text-muted">{t("bots.app.grants.noChannels")}</p>
        ) : (
          ceiling.channels.map((c) => (
            <Checkbox
              key={c.id}
              data-testid={`${testId}-channel-${c.id}`}
              label={`#${c.name}`}
              checked={!narrowing.droppedChannelIds.includes(c.id)}
              disabled={disabled}
              onChange={(on) =>
                onChange({ ...narrowing, droppedChannelIds: toggle(narrowing.droppedChannelIds, c.id, !on) })
              }
            />
          ))
        )}
      </fieldset>

      {ceiling.history > 0 && (
        <TextInput
          type="number"
          data-testid={`${testId}-history`}
          label={t("bots.app.grants.history", { max: ceiling.history })}
          value={historyText}
          disabled={disabled}
          onChange={setHistory}
        />
      )}

      {ceiling.egress.length > 0 && (
        <fieldset className="flex flex-col gap-2" disabled={disabled}>
          <legend className="mb-2 text-secondary font-medium text-fg">{t("bots.app.grants.egress")}</legend>
          {ceiling.egress.map((host) => (
            <Switch
              key={host}
              id={`${testId}-egress-${host}`}
              data-testid={`${testId}-egress-${host}`}
              label={host}
              checked={!narrowing.droppedEgress.includes(host)}
              disabled={disabled}
              onChange={(on) => onChange({ ...narrowing, droppedEgress: toggle(narrowing.droppedEgress, host, !on) })}
            />
          ))}
        </fieldset>
      )}
    </div>
  );
};
