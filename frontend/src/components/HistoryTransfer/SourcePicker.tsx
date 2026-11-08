import React from "react";
import { useTranslation } from "react-i18next";
import { RadioList, type RadioOption } from "../ui/RadioList";
import * as api from "../../services/api";
import { isSupported, sortSources } from "../../utils/historyTransfer";
import { formatDateTime } from "../../utils/format";

interface SourcePickerProps {
  sources: api.TransferSource[];
  value: string | null;
  onChange: (deviceId: string) => void;
}

/** This account's other devices as a pick-one list: online first, then most recently active; a device too old to send is shown, disabled. */
export const SourcePicker: React.FC<SourcePickerProps> = ({ sources, value, onChange }) => {
  const { t } = useTranslation("settings");
  const options: RadioOption[] = sortSources(sources).map((source) => {
    let detail: string;
    if (!isSupported(source)) {
      detail = t("historyTransfer.needsUpdate");
    } else if (source.online) {
      detail = t("historyTransfer.online");
    } else if (source.last_active_at) {
      detail = t("historyTransfer.lastActive", { time: formatDateTime(source.last_active_at) });
    } else {
      detail = t("historyTransfer.notSeen");
    }
    return {
      value: source.device_id,
      label: source.device_name ?? t("historyTransfer.unnamedDevice"),
      detail,
      disabled: !isSupported(source),
    };
  });
  return (
    <RadioList
      aria-label={t("historyTransfer.pickAriaLabel")}
      data-testid="history-source-list"
      options={options}
      value={value}
      onChange={onChange}
    />
  );
};

/** The device to preselect: the first one that can send. */
export function defaultSource(sources: api.TransferSource[]): string | null {
  return sortSources(sources).find(isSupported)?.device_id ?? null;
}
