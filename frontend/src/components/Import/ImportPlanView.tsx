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
  EmojiPlan,
  ImportPlan,
  ImportSource,
  OverwritePlan,
  PersonPlan,
  PlanNote,
  RolePlan,
  SkipReason,
} from "../../types/import";

interface ImportPlanViewProps {
  plan: ImportPlan;
  inviteSheet: string;
}

/** The dry run, section by section: what lands where, and everything that doesn't survive (#1180, #1179). */
export const ImportPlanView: React.FC<ImportPlanViewProps> = ({ plan, inviteSheet }) => {
  const { t } = useTranslation("import");
  const source = t(`sourceName.${plan.source}`);
  const blocks = channelBlocks(plan);
  const humans = plan.people.filter((p) => !p.is_bot);
  const bots = plan.people.filter((p) => p.is_bot);

  return (
    <div className="flex flex-col gap-8" data-testid="import-plan">
      <Section title={t("plan.channelsTitle")} testId="import-plan-channels">
        {plan.totals.overwrites > 0 && (
          <p className="text-secondary text-muted">{t("plan.overwritesNote", { source })}</p>
        )}
        {blocks.map((block) => (
          <div key={block.name ?? ""} className="flex flex-col gap-1" data-testid="import-plan-block">
            {/* A source without categories has one block; it needs no heading. */}
            {(block.name !== null || plan.categories.length > 0) && (
              <p className="flex items-baseline gap-2 px-1 text-secondary font-medium text-muted">
                <span>{block.name ?? t("plan.uncategorised")}</span>
                {block.exists && <span className="font-normal">{t("plan.exists")}</span>}
              </p>
            )}
            {block.channels.length === 0 && (
              <p className="px-3 text-secondary text-muted">{t("plan.emptyCategory")}</p>
            )}
            {block.channels.map((c) => (
              <ChannelRow key={c.source_id} channel={c} source={source} />
            ))}
          </div>
        ))}
      </Section>

      {plan.skipped_channels.length > 0 && (
        <Section title={t("plan.skippedTitle")} testId="import-plan-skipped">
          {plan.skipped_channels.map((s, i) => (
            <Row key={`${s.name}-${i}`} testId={`import-skipped-${i}`} label={s.name} detail={skipReason(t, s.reason)} />
          ))}
        </Section>
      )}

      {plan.emoji.length > 0 && (
        <Section title={t("plan.emojiTitle")} testId="import-plan-emoji">
          {plan.emoji.map((e) => (
            <Row
              key={e.source_id}
              testId={`import-emoji-${e.source_id}`}
              label={e.source_name}
              value={e.shortcode ? `:${e.shortcode}:` : t("plan.emojiSkipped")}
              valueMachine={!!e.shortcode}
              tag={e.exists ? t("plan.exists") : undefined}
              detail={emojiDetail(t, plan.source, e)}
            />
          ))}
        </Section>
      )}

      {plan.roles.length > 0 && (
        <Section title={t("plan.rolesTitle")} testId="import-plan-roles">
          <p className="text-secondary text-muted">
            {plan.permissions_known ? t("plan.rolesLead") : t("plan.rolesUnknown")}
          </p>
          {plan.roles.map((r) => (
            <Row
              key={r.source_id}
              testId={`import-role-${r.source_id}`}
              label={r.name}
              detail={t("plan.roleHolders", { count: r.holders })}
              value={roleTarget(t, r)}
            />
          ))}
        </Section>
      )}

      {plan.user_groups.length > 0 && (
        <Section title={t("plan.userGroupsTitle")} testId="import-plan-user-groups">
          <p className="text-secondary text-muted">{t("plan.userGroupsLead")}</p>
          {plan.user_groups.map((g) => (
            <Row
              key={g.source_id}
              testId={`import-user-group-${g.source_id}`}
              label={`@${g.handle}`}
              value={g.name}
              detail={t("plan.userGroupMembers", { count: g.members.length, names: g.members.join(", ") })}
            />
          ))}
        </Section>
      )}

      <Section title={t("plan.peopleTitle")} testId="import-plan-people">
        <p className="text-secondary text-muted">{t(`${plan.source}.peopleLead`)}</p>
        {humans.map((p) => (
          <Row
            key={p.source_id}
            testId={`import-person-${p.source_id}`}
            label={p.display_name === p.username ? p.username : `${p.display_name} (${p.username})`}
            value={p.admin ? t("plan.makeAdmin") : undefined}
            detail={personDetail(t, plan, p)}
          />
        ))}
        {bots.map((p) => (
          <Row key={p.source_id} label={p.username} detail={t("plan.bot")} />
        ))}
        <div>
          <CopySheetButton sheet={inviteSheet} />
        </div>
      </Section>

      <Section title={t("plan.notesTitle")} testId="import-plan-notes">
        <ul className="flex list-disc flex-col gap-1 ps-5 text-secondary text-dim">
          {plan.notes.map((n) => (
            <li key={n.kind}>{planNote(t, n, source)}</li>
          ))}
        </ul>
      </Section>

      {plan.warnings.length > 0 && (
        <Section title={t("plan.warningsTitle")} testId="import-plan-warnings">
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

const ChannelRow: React.FC<{ channel: ChannelPlan; source: string }> = ({ channel: c, source }) => {
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
          {c.kind === "voice" && <span className="sr-only">{t("plan.voice")}</span>}
        </span>
        {c.exists && <span className="shrink-0 text-secondary text-dim">{t("plan.exists")}</span>}
      </div>
      {c.description && <span className="whitespace-pre-line text-secondary text-muted">{c.description}</span>}
      {c.notes.map((n, i) => (
        <span key={i} className="text-secondary text-muted">
          {channelNote(t, n, source)}
        </span>
      ))}
      {c.private && (
        <span
          className="flex items-center gap-1.5 text-secondary font-medium text-fg"
          data-testid={`import-channel-private-${c.source_id}`}
        >
          <EyeOff size={13} aria-hidden="true" className="shrink-0" />
          {t("plan.privateOnSource", { source })}
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
    state === "copied" ? t("plan.copiedSheet") : state === "failed" ? t("plan.copySheetFailed") : t("plan.copySheet");
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

function channelNote(t: TFunction<"import">, n: ChannelNote, source: string): string {
  switch (n.kind) {
    case "announcement":
      return t("plan.noteAnnouncement");
    case "forum":
      return t("plan.noteForum");
    case "stage":
      return t("plan.noteStage");
    case "renamed":
      return t("plan.noteRenamed", { from: n.from });
    case "archived":
      return t("plan.noteArchived", { source });
  }
}

function overwrite(t: TFunction<"import">, o: OverwritePlan): string {
  const list = (ps: OverwritePlan["allow"]) => ps.map((p) => t(`plan.perm.${p}`)).join(", ");
  const rules: string[] = [];
  if (o.allow.length > 0) {
    rules.push(t("plan.overwriteAllowed", { list: list(o.allow) }));
  }
  if (o.deny.length > 0) {
    rules.push(t("plan.overwriteDenied", { list: list(o.deny) }));
  }
  if (o.other_bits > 0) {
    rules.push(t("plan.overwriteOther", { count: o.other_bits }));
  }
  const key = o.member ? "plan.overwriteMember" : "plan.overwriteRole";
  return t(key, { target: o.target, rules: rules.join("; ") });
}

/** The quieter line under an emoji: what it is another name for, and what happened to it. */
function emojiDetail(t: TFunction<"import">, source: ImportSource, e: EmojiPlan): string | undefined {
  if (e.note?.kind === "alias_target_missing") {
    return t("plan.emojiAliasMissing", { target: e.alias_of ?? "" });
  }
  const parts: string[] = [];
  if (e.alias_of) {
    parts.push(t("plan.emojiAlias", { target: e.alias_of }));
  }
  if (e.note) {
    parts.push(emojiNote(t, source, e.note));
  }
  return parts.join(". ") || undefined;
}

function emojiNote(t: TFunction<"import">, source: ImportSource, n: EmojiNote): string {
  switch (n.kind) {
    case "renamed":
      return t("plan.emojiRenamed", { taken: n.collided_with });
    case "no_valid_shortcode":
      return t("plan.emojiNoShortcode");
    case "image_remote":
      return t(`${source}.emojiImageRemote`);
    case "image_missing":
      return t("plan.emojiImageMissing");
    case "alias_target_missing":
      return t("plan.emojiAliasMissing", { target: "" });
  }
}

function skipReason(t: TFunction<"import">, r: SkipReason): string {
  switch (r.kind) {
    case "orphan_thread":
      return t("plan.skipOrphanThread");
    case "direct":
      return t("plan.skipDirect");
    case "unsupported":
      return t("plan.skipUnsupported", { type: r.source_type });
    case "archived":
      return t("plan.skipArchived");
  }
}

function roleTarget(t: TFunction<"import">, r: RolePlan): string {
  switch (r.maps_to) {
    case "admin":
      return t("plan.roleAdmin");
    case "member":
      return t("plan.roleMember");
    case "discarded":
      return t("plan.roleDiscarded");
    case "bot":
      return t("plan.roleBot");
  }
}

/** Their email when the source shares emails (how they'll be reached), and the channels they belong in. */
function personDetail(t: TFunction<"import">, plan: ImportPlan, p: PersonPlan): string | undefined {
  const parts: string[] = [];
  if (plan.totals.people_with_email > 0) {
    parts.push(p.email ?? t("plan.noEmail"));
  }
  if (p.channels.length > 0) {
    parts.push(p.channels.join(", "));
  }
  return parts.join(" · ") || undefined;
}

function planNote(t: TFunction<"import">, n: PlanNote, source: string): string {
  switch (n.kind) {
    case "no_emails":
      return t("plan.noteNoEmails", { source });
    case "email_invites":
      return t("plan.noteEmailInvites", { count: n.with_email });
    case "no_snapshot":
      return t("discord.noteNoSnapshot");
    case "no_workspace_snapshot":
      return t("slack.noteNoWorkspaceSnapshot");
    case "archived_skipped":
      return t("plan.noteArchivedSkipped", { count: n.count });
    case "user_groups_not_imported":
      return t("plan.noteUserGroups", { count: n.count });
    case "private_channels":
      return t("plan.notePrivate", { count: n.count, source });
    case "roles_collapsed":
      return t("plan.noteRoles", { count: n.discarded });
    case "history_not_imported":
      return t("plan.noteHistory", { count: n.messages, source });
  }
}
