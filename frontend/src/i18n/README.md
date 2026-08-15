# Localization (i18n)

`i18next` + `react-i18next`. Catalogues are plain JSON under
`locales/<language>/<namespace>.json`, bundled eagerly at build time.

Canonical article: [`.codesight/wiki/i18n.md`](../../../.codesight/wiki/i18n.md).
That one covers the design decisions; this one is the working checklist.

---

## Adding a locale

1. **Copy the English catalogue.**

   ```bash
   cp -r frontend/src/i18n/locales/en frontend/src/i18n/locales/de
   ```

   Copy — never start from an empty directory. Copying gives you every key
   that exists, so "which strings still need doing" is a diff against `en`
   rather than a hunt through the source.

2. **Translate the values.** Never touch the keys. Never add keys that `en`
   does not have — a key only `de` has is a key no other locale can ever be
   checked against.

3. **Register the language** in `languages.ts`:

   ```ts
   { code: "de", label: "Deutsch", dir: "ltr" },
   ```

   `label` is the **endonym** — the language's own name for itself. "German"
   is useless to the person reaching for this control. `dir` is `rtl` only for
   Arabic/Hebrew-script languages.

   `code` is a **base tag only** — `zh`, never `zh-Hans`; `pt`, never `pt-BR`.
   A subtagged code cannot work here from either end: `normalizeLanguage`
   lowercases every tag it is handed, while i18next canonicalizes codes
   through `Intl.getCanonicalLocales` before testing them against
   `supportedLngs`, so `zh-hans` is rejected by i18next and `zh-Hans` is
   rejected by `normalizeLanguage`. Either way the catalogue silently renders
   English. Base tags are also what real OS locales degrade to — `zh-CN` and
   `zh-Hans-CN` both reduce to `zh` — so the subtag would not have matched the
   machines you are shipping for anyway. Script differences that matter (`zh`
   Simplified vs Traditional) are one locale per catalogue, distinguished by
   the endonym, and would need `normalizeLanguage` reworked to ship both.
   Setting `dir: "rtl"` flips `<html dir>` **and** the component layout, which
   is written with logical properties throughout (#855). You are inheriting
   that work, not redoing it — but read the RTL section of the wiki article
   before writing any new UI, because the conventions there (logical utilities,
   `.rtl-mirror` / `.rtl-unmirror`, `<bdi>` vs `dir="ltr"`) are what keep it
   true, and `e2e/rtl.spec.ts` is what catches you if they slip.

4. **Check the plural forms.** See below — this is the step that gets skipped
   and it is the one that cannot be fixed later without a re-translation.

Nothing else needs editing. The selector, `supportedLngs`, the OS-locale probe
and the resource loader all read from `SUPPORTED_LANGUAGES`.

## Key conventions

**Address keys as `namespace:section.item`.**

```tsx
const { t } = useTranslation("settings");
t("language.heading");

// or, from another namespace:
t("common:actions.cancel");
```

- **Keys are stable identifiers, never the English text.** `auth:login.submit`,
  not `"Sign in"`. Copy changes then stay copy changes instead of silently
  orphaning every translation of that string.
- **camelCase** for the leaf, dot-separated sections. One or two levels of
  nesting; three is a sign the namespace should have been split.
- **Namespace by feature area**, per `namespaces.ts`. `common` is for genuinely
  shared verbs and nouns (Cancel, Save, Close). If a string belongs to one
  feature, it goes in that feature's file even if it happens to read the same
  as another.

## Interpolation — never concatenate

```tsx
// Right
t("channels:members.removedBy", { name: actor });
//   "Removed by {{name}}"

// Wrong — word order is not universal, and this cannot be translated at all
`Removed by ${actor}`;
```

## Plurals — every count-bearing string, without exception

Any string that interpolates a count MUST be a plural key, **even when English
needs only the two forms**. English gets `_one` / `_other`; Russian, Ukrainian
and Arabic have categories English does not (`_few`, `_many`, `_zero`), and a
non-plural key gives their translators nowhere to put them.

```json
{
  "members": {
    "count_one": "{{count}} member",
    "count_other": "{{count}} members"
  }
}
```

```tsx
t("channels:members.count", { count: members.length });
```

i18next selects the suffix from the active language's CLDR plural rules, so a
translator adding `count_few` / `count_many` for `ru` needs no code change.

**The suffix set is per language, not copied from `en`.** CLDR gives Chinese
one category, so `zh` carries `count_other` and no `count_one` at all —
mirroring English's two forms there would leave a `_one` entry i18next can
never select. Its key count is therefore legitimately lower than `en`'s: one
key per plural family instead of two. Check the language's categories with
`new Intl.PluralRules("<code>").resolvedOptions().pluralCategories` and write
exactly those.

**A count rendered next to a static noun is still a count-bearing string.**
`<b>{n}</b> members` must be one plural key, not a number glued to `t("members")`.

**Arabic needs all six**: `_zero`, `_one`, `_two`, `_few` (n%100 = 3–10),
`_many` (n%100 = 11–99), `_other`. English's 38 plural families become 228
`ar` keys. A family missing one category does not error — it falls back to
English mid-sentence — so check every family, not a sample.

`{{count}}` is plain interpolation, never `Intl.NumberFormat`: it renders
ASCII digits in every locale, `ar` included. Write literal digits in a
catalogue as ASCII to match.

## What is NOT translated

- `console.log` / `console.warn` / `console.error` and anything built for them.
- `Error` messages destined only for logs or crash reports.
- `data-testid` values — tests must not move when copy does.
- Developer-facing strings, debug panels, dev-only branches.
- Protocol / wire values, command names, preference keys, CSS class names —
  **as values**. The moment one is RENDERED to a user it is copy: `admin`,
  `member`, `excellent`, `strict` all reach the screen and all need keys. Map
  the token to literal `t("…")` calls, one per value; a key built from the
  value with a template literal is invisible to `i18n-check`.
- Keyboard GLYPHS — `⌘ ⌥ ⇧`, the arrows `↑ ↓ ← →`, the backtick. Key NAMES
  that are words (Ctrl, Shift, Esc, Space…) live in `common:keys.*` and are
  translated; see `keyboard/keyCombo.ts`.

## Fallback behaviour

`fallbackLng: "en"` with `returnEmptyString: false`. A key missing from the
active language — or present but left as `""` in a half-finished catalogue —
renders the **English** string, not the raw key. A partial translation is
therefore always shippable.

`node scripts/i18n-check.mjs` treats that as a **warning**, not a failure: it
prints a per-locale count of keys still awaiting translation and exits 0. New
copy lands English-only on purpose, and a translation pass clears the backlog
later. What still FAILS the check is a catalogue that is wrong rather than
behind — a key `en` does not have, a plural family you started but did not
finish, placeholder drift, or an empty string.

## Where the preference lives

Device-local (`localStorage`, via `storage.ts`), mirroring the device-local
font size in `colorUtils.ts` — not the synced preferences blob. The synced blob
requires a signed-in user and an unlocked local DB, so it cannot reach the
login, PIN and enrollment screens at all. Those are the first screens a user
who does not read English sees.
