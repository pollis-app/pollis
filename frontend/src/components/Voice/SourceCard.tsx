import React from "react";
import { Card } from "../ui/Card";
import { PressButton } from "../ui/PressButton";
import { FOCUS_RING } from "../ui/focusRing";

interface SourceCardProps {
  disabled: boolean;
  onPick: () => void;
  testId: string;
  title: string;
  subtitle?: string;
  /** PNG data URL; the capture-helper path has none, so `icon` shows. */
  thumbnail?: string;
  icon: React.ReactNode;
}

/** One pickable capture source, a screen, window or camera (#1967): a 16:10 preview over its name. Shared by `ScreenSharePicker` and `CameraPicker`. */
export const SourceCard: React.FC<SourceCardProps> = ({
  disabled,
  onPick,
  testId,
  title,
  subtitle,
  thumbnail,
  icon,
}) => (
  <PressButton
    activateOn="click"
    onClick={onPick}
    disabled={disabled}
    data-testid={testId}
    className={`min-h-[6.25rem] text-start font-mono text-secondary disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer rounded-panel ${FOCUS_RING}`}
  >
    <Card padding="none" className="flex flex-col items-stretch h-full overflow-hidden">
      {/* Fixed 16:10 (the requested thumbnail size) keeps the grid even. */}
      <div className="flex-1 flex items-center justify-center overflow-hidden bg-bg text-muted aspect-[16/10]">
        {thumbnail ? (
          // alt="": the title labels it. object-contain avoids cropping.
          <img
            src={thumbnail}
            alt=""
            className="w-full h-full object-contain"
            draggable={false}
          />
        ) : (
          icon
        )}
      </div>
      <div className="p-2">
        <div className="truncate text-fg" title={title}>
          {title}
        </div>
        {subtitle ? (
          <div className="truncate text-muted text-meta">
            {subtitle}
          </div>
        ) : null}
      </div>
    </Card>
  </PressButton>
);
