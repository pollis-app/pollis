import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { Volume2, Hash, EyeOff } from "lucide-react";
import { writeClipboardText } from "../../bridge";
import { Button } from "../ui/Button";
import type {
  ChannelNote,
  ChannelPlan,
  EmojiNote,
  ImportPlan,
  OverwritePlan,
  PlanNote,
  RolePlan,
  SkipReason,
} from "../../types/import";

interface ImportPlanViewProps {
  plan: ImportPlan;
  inviteSheet: string;
}

/** The dry run, section by section: what lands where, and everything that doesn't survive (#1180). */
export const ImportPlanView: React.FC<ImportPlanViewProps> = ({ plan, inviteSheet }) => {
  const { t } = useTranslation("import");
  const blocks = channelBlocks(plan);
  const humans = plan.people.filter((p) => !p.is_bot);
  const bots = plan.people.filter((p) => p.is_bot);

  return (
    <div className="flex flex-col gap-8" data-testid="import-plan">
      <Section title={t("discord.channelsTitle")} testId="import-plan-channels">
        {plan.totals.overwrites > 0 && (
          <p className="text-secondary text-muted">{t("discord.overwritesNote")}</p>
        )}
        {blocks.map((block) => (
          <div key={block.name ?? ""} className="flex flex-col gap-1" data-testid="import-plan-block">
            <p className="flex items-baseline gap-2 px-1 text-secondary font-medium text-muted">
              <span>{block.name ?? t("discord.uncategorised")}</span>
              {block.exists && <span className="font-normal">{t("discord.exists")}</span>}
            </p>
            {block.channels.length === 0 && (
              <p className="px-3 text-secondary text-muted">{t("discord.emptyCategory")}</p>
            )}
            {block.channels.map((c) => (
              <ChannelRow key={c.source_id} channel={c} />
            ))}
          </div>
        ))}
      </Section>

      {plan.skipped_channels.length > 0 && (
        <Section title={t("discord.skippedTitle")} testId="import-plan-skipped">
          {plan.skipped_channels.map((s, i) => (
            <Row key={`${s.name}-${i}`} label={s.name} detail={skipReason(t, s.reason)} />
          ))}
        </Section>
      )}

      {plan.emoji.length > 0 && (
        <Section title={t("discord.emojiTitle")} testId="import-plan-emoji">
          {plan.emoji.map((e) => (
            <Row
              key={e.source_id}
              testId={`import-emoji-${e.source_id}`}
              label={e.source_name}
              value={e.shortcode ? `:${e.shortcode}:` : t("discord.emojiSkipped")}
              valueMachine={!!e.shortcode}
              tag={e.exists ? t("discord.exists") : undefined}
              detail={e.note ? emojiNote(t, e.note) : undefined}
            />
          ))}
        </Section>
      )}

      {plan.roles.length > 0 && (
        <Section title={t("discord.rolesTitle")} testId="import-plan-roles">
          <p className="text-secondary text-muted">
            {plan.permissions_known ? t("discord.rolesLead") : t("discord.rolesUnknown")}
          </p>
          {plan.roles.map((r) => (
            <Row
              key={r.source_id}
              testId={`import-role-${r.source_id}`}
              label={r.name}
              detail={t("discord.roleHolders", { count: r.holders })}
              value={roleTarget(t, r)}
            />
          ))}
        </Section>
      )}

      <Section title={t("discord.peopleTitle")} testId="import-plan-people">
        <p className="text-secondary text-muted">{t("discord.peopleLead")}</p>
        {humans.map((p) => (
          <Row
            key={p.source_id}
            testId={`import-person-${p.source_id}`}
            label={p.display_name === p.username ? p.username : `${p.display_name} (${p.username})`}
            value={p.admin ? t("discord.makeAdmin") : undefined}
            detail={p.channels.length > 0 ? p.channels.join(", ") : undefined}
          />
        ))}
        {bots.map((p) => (
          <Row key={p.source_id} label={p.username} detail={t("discord.bot")} />
        ))}
        <div>
          <CopySheetButton sheet={inviteSheet} />
        </div>
      </Section>

      <Section title={t("discord.notesTitle")} testId="import-plan-notes">
        <ul className="flex list-disc flex-col gap-1 ps-5 text-secondary text-dim">
          {plan.notes.map((n) => (
            <li key={n.kind}>{planNote(t, n)}</li>
          ))}
        </ul>
      </Section>

      {plan.warnings.length > 0 && (
        <Section title={t("discord.warningsTitle")} testId="import-plan-warnings">
          <ul className="flex list-disc flex-col gap-1 ps-5 text-secondary text-muted font-machine break-all">
            {plan.warnings.map((w, i) => (
              <li key={i}>{w}</li>
            ))}
          </ul>
        </Section>
      )}
    </div>
  );
};

const Section: React.FC<{ title: string; testId: string; children: React.ReactNode }> = ({ title, testId, children }) => (
  <section className="flex flex-col gap-2" data-testid={testId}>
    <h3 className="text-heading font-semibold text-fg">{title}</h3>
    {children}
  </section>
);

interface RowProps {
  label: string;
  value?: string;
  valueMachine?: boolean;
  tag?: string;
  detail?: string;
  testId?: string;
}

/** One filled row: a name, what it becomes, and a quieter line under it. */
const Row: React.FC<RowProps> = ({ label, value, valueMachine, tag, detail, testId }) => (
  <div className="flex flex-col gap-0.5 rounded-control bg-surface-raised px-3 py-2" data-testid={testId}>
    <div className="flex items-baseline justify-between gap-3">
      <span className="min-w-0 truncate text-body text-fg">{label}</span>
      {(value || tag) && (
        <span className={`shrink-0 text-secondary text-dim ${valueMachine ? "font-machine" : ""}`}>
          {[value, tag].filter(Boolean).join(" · ")}
        </span>
      )}
    </div>
    {detail && <span className="text-secondary text-muted">{detail}</span>}
  </div>
);

const ChannelRow: React.FC<{ channel: ChannelPlan }> = ({ channel: c }) => {
  const { t } = useTranslation("import");
  const Icon = c.kind === "voice" ? Volume2 : Hash;
  return (
    <div
      className="flex flex-col gap-0.5 rounded-control bg-surface-raised px-3 py-2"
      data-testid={`import-channel-${c.source_id}`}
      data-exists={c.exists}
    >
      <div className="flex items-center justify-between gap-3">
        <span className="flex min-w-0 items-center gap-1.5 text-body text-fg">
          <Icon size={14} aria-hidden="true" className="shrink-0 text-muted" />
          <span className="truncate">{c.name}</span>
          {c.kind === "voice" && <span className="sr-only">{t("discord.voice")}</span>}
        </span>
        {c.exists && <span className="shrink-0 text-secondary text-dim">{t("discord.exists")}</span>}
      </div>
      {c.description && <span className="text-secondary text-muted">{c.description}</span>}
      {c.notes.map((n, i) => (
        <span key={i} className="text-secondary text-muted">
          {channelNote(t, n)}
        </span>
      ))}
      {c.private && (
        <span
          className="flex items-center gap-1.5 text-secondary font-medium text-fg"
          data-testid={`import-channel-private-${c.source_id}`}
        >
          <EyeOff size={13} aria-hidden="true" className="shrink-0" />
          {t("discord.privateOnDiscord")}
        </span>
      )}
      {c.overwrites.map((o, i) => (
        <span key={i} className="text-secondary text-muted">
          {overwrite(t, o)}
        </span>
      ))}
    </div>
  );
};

type CopyState = "idle" | "copied" | "failed";

const CopySheetButton: React.FC<{ sheet: string }> = ({ sheet }) => {
  const { t } = useTranslation("import");
  const [state, setState] = useState<CopyState>("idle");
  const copy = async () => {
    try {
      setState((await writeClipboardText(sheet)) ? "copied" : "failed");
    } catch {
      setState("failed");
    }
  };
  const label =
    state === "copied" ? t("discord.copiedSheet") : state === "failed" ? t("discord.copySheetFailed") : t("discord.copySheet");
  return (
    <Button variant="ghost" size="sm" onClick={copy} data-testid="import-copy-sheet" data-copy-state={state}>
      {label}
    </Button>
  );
};

interface Block {
  name: string | null;
  exists: boolean;
  channels: ChannelPlan[];
}

/** The uncategorised block, then each category in order, empty ones included. */
function channelBlocks(plan: ImportPlan): Block[] {
  const blocks: Block[] = [];
  const loose = plan.channels.filter((c) => c.category === null);
  if (loose.length > 0) {
    blocks.push({ name: null, exists: false, channels: loose });
  }
  for (const k of plan.categories) {
    blocks.push({ name: k.name, exists: k.exists, channels: plan.channels.filter((c) => c.category === k.name) });
  }
  return blocks;
}

function channelNote(t: TFunction<"import">, n: ChannelNote): string {
  switch (n.kind) {
    case "announcement":
      return t("discord.noteAnnouncement");
    case "forum":
      return t("discord.noteForum");
    case "stage":
      return t("discord.noteStage");
    case "renamed":
      return t("discord.noteRenamed", { from: n.from });
  }
}

function overwrite(t: TFunction<"import">, o: OverwritePlan): string {
  const list = (ps: OverwritePlan["allow"]) => ps.map((p) => t(`discord.perm.${p}`)).join(", ");
  const rules: string[] = [];
  if (o.allow.length > 0) {
    rules.push(t("discord.overwriteAllowed", { list: list(o.allow) }));
  }
  if (o.deny.length > 0) {
    rules.push(t("discord.overwriteDenied", { list: list(o.deny) }));
  }
  if (o.other_bits > 0) {
    rules.push(t("discord.overwriteOther", { count: o.other_bits }));
  }
  const key = o.member ? "discord.overwriteMember" : "discord.overwriteRole";
  return t(key, { target: o.target, rules: rules.join("; ") });
}

function emojiNote(t: TFunction<"import">, n: EmojiNote): string {
  switch (n.kind) {
    case "renamed":
      return t("discord.emojiRenamed", { taken: n.collided_with });
    case "no_valid_shortcode":
      return t("discord.emojiNoShortcode");
    case "image_remote":
      return t("discord.emojiImageRemote");
    case "image_missing":
      return t("discord.emojiImageMissing");
  }
}

function skipReason(t: TFunction<"import">, r: SkipReason): string {
  switch (r.kind) {
    case "orphan_thread":
      return t("discord.skipOrphanThread");
    case "direct":
      return t("discord.skipDirect");
    case "unsupported":
      return t("discord.skipUnsupported", { type: r.source_type });
  }
}

function roleTarget(t: TFunction<"import">, r: RolePlan): string {
  switch (r.maps_to) {
    case "admin":
      return t("discord.roleAdmin");
    case "member":
      return t("discord.roleMember");
    case "discarded":
      return t("discord.roleDiscarded");
    case "bot":
      return t("discord.roleBot");
  }
}

function planNote(t: TFunction<"import">, n: PlanNote): string {
  switch (n.kind) {
    case "no_emails":
      return t("discord.noteNoEmails");
    case "no_snapshot":
      return t("discord.noteNoSnapshot");
    case "private_channels":
      return t("discord.notePrivate", { count: n.count });
    case "roles_collapsed":
      return t("discord.noteRoles", { count: n.discarded });
    case "history_not_imported":
      return t("discord.noteHistory", { count: n.messages });
  }
}
