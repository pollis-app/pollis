import React from "react";
import { useTranslation } from "react-i18next";
import { SuggestList } from "./SuggestList";
import { BotBadge } from "./BotBadge";
import { commandKey, type GroupCommand } from "../../utils/slashCommands";

interface CommandSuggestListProps {
  commands: GroupCommand[];
  activeIndex: number;
  query: string;
  onSelect: (command: GroupCommand) => void;
  onHover: (index: number) => void;
}

/**
 * Modern skin's slash-command list (#2018), the same shell as mentions: name, argument
 * hint, one-line description, and the App's name with the Bot badge when two Apps share
 * the name (`/ticket · linear`). ChatInput owns the keys.
 */
export const CommandSuggestList: React.FC<CommandSuggestListProps> = ({
  commands,
  activeIndex,
  query,
  onSelect,
  onHover,
}) => {
  const { t } = useTranslation("common");

  return (
    <SuggestList
      items={commands}
      activeIndex={activeIndex}
      query={query}
      testId="command-suggest-list"
      label={t("commands.listLabel")}
      matchingKey="commands.matching"
      itemKey={commandKey}
      optionTestId={(c) => `command-option-${c.command}-${c.botUsername}`}
      onSelect={onSelect}
      onHover={onHover}
      renderItem={(c) => (
        <>
          <span className="flex-shrink-0 font-mono text-fg">/{c.command}</span>
          {c.argHint && <span className="flex-shrink-0 font-mono text-secondary text-muted">{c.argHint}</span>}
          <span className="truncate text-secondary text-muted">{c.description}</span>
          {c.needsTarget && (
            <span
              data-testid={`command-option-app-${c.command}-${c.botUsername}`}
              className="ms-auto flex flex-shrink-0 items-center gap-1 text-secondary text-dim"
            >
              <span>· {c.appName}</span>
              <BotBadge data-testid={`command-option-bot-${c.command}-${c.botUsername}`} />
            </span>
          )}
        </>
      )}
    />
  );
};
