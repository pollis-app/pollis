/**
 * BCP-47 tag → registered language code (#1725). Pure; `mobile/i18n/languageTag.ts` is a byte copy
 * (mobile/tests/i18n.test.ts fails if they differ).
 *
 * A registered code is a lowercase base tag (`zh`, `pt`) or, for a language shipped in more than one
 * script, `<base>-<Script>` in canonical case (`pa-Arab`, `pa-Guru`). Only the languages in
 * SCRIPT_VARIANTS resolve to a script code; every other language keeps the base-tag rule, so
 * `zh-Hant-TW` is still `zh` and every stored preference written before script codes existed still
 * resolves to itself.
 */

/** Languages shipped per script: an explicit script subtag wins, then the region, then `fallback`. */
export const SCRIPT_VARIANTS: Readonly<Record<string, { byRegion: Readonly<Record<string, string>>; fallback: string }>> = {
  // Punjabi: Shahmukhi (Arabic script, RTL) in Pakistan, Gurmukhi (LTR) in India and by default.
  pa: { byRegion: { PK: "Arab", IN: "Guru" }, fallback: "Guru" },
};

/** The shape a registered code must have: `xx`, `xxx`, or `xx-Scrp`. */
export const REGISTERED_CODE = /^[a-z]{2,3}(-[A-Z][a-z]{3})?$/;

function titleCase(script: string): string {
  return script.charAt(0).toUpperCase() + script.slice(1).toLowerCase();
}

/** Reduce a tag to one of `codes`, or null: `pt-BR` → `pt`, `pa-PK` → `pa-Arab`, `zh-Hans-CN` → `zh`. */
export function matchLanguageTag(raw: string | null | undefined, codes: readonly string[]): string | null {
  const tag = raw?.trim();
  if (!tag) {
    return null;
  }
  const exact = codes.find((c) => c.toLowerCase() === tag.toLowerCase().replace(/_/g, "-"));
  if (exact) {
    return exact;
  }
  const [first, ...rest] = tag.split(/[-_]/);
  const base = first.toLowerCase();
  const variants = SCRIPT_VARIANTS[base];
  if (variants) {
    const script = rest.find((p) => /^[A-Za-z]{4}$/.test(p));
    const region = rest.find((p) => /^([A-Za-z]{2}|\d{3})$/.test(p))?.toUpperCase();
    const chosen = script ? titleCase(script) : (region && variants.byRegion[region]) || variants.fallback;
    const code = `${base}-${chosen}`;
    // Never hand a reader the other script: a Shahmukhi reader cannot read Gurmukhi.
    return codes.includes(code) ? code : null;
  }
  return codes.includes(base) ? base : null;
}
