/* The saved palette paints the first frame, and every surface follows a change (#1447): the load path, then the wiring. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { basename, dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

import {
  ACCENT_STORE_KEY,
  accentFromPreferences,
  accentOnUserChange,
  loadBootAccent,
  normalizeAccentHex,
  persistAccent,
  type AccentBackend,
} from "../theme/accentStore.ts";
import { ACCENT_PRESETS, DEFAULT_ACCENT_HEX } from "../theme/accents.ts";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const LILAC = ACCENT_PRESETS.find((p) => p.n === "Lilac")!.c;

function memoryStore(initial: Record<string, string> = {}) {
  const data = new Map(Object.entries(initial));
  const backend: AccentBackend = {
    getItem: (k) => data.get(k) ?? null,
    setItem: (k, v) => {
      data.set(k, v);
    },
  };
  return { data, backend };
}

test("a cold start paints the stored accent", () => {
  const { backend } = memoryStore({ [ACCENT_STORE_KEY]: LILAC });
  assert.equal(loadBootAccent(backend), LILAC);
});

test("the stored accent is normalized", () => {
  const { backend } = memoryStore({ [ACCENT_STORE_KEY]: " #BDA3E0 " });
  assert.equal(loadBootAccent(backend), "#bda3e0");
});

test("nothing stored, garbage stored or no store at all → the default", () => {
  assert.equal(loadBootAccent(memoryStore().backend), DEFAULT_ACCENT_HEX);
  for (const junk of ["", "purple", "#12345", "#gggggg", "#1234567"]) {
    const { backend } = memoryStore({ [ACCENT_STORE_KEY]: junk });
    assert.equal(loadBootAccent(backend), DEFAULT_ACCENT_HEX, JSON.stringify(junk));
  }
  assert.equal(loadBootAccent(null), DEFAULT_ACCENT_HEX);
  assert.equal(loadBootAccent(undefined), DEFAULT_ACCENT_HEX);
});

test("a store that throws on read never breaks the launch", () => {
  const backend: AccentBackend = {
    getItem: () => {
      throw new Error("keychain unavailable");
    },
    setItem: () => {},
  };
  assert.equal(loadBootAccent(backend), DEFAULT_ACCENT_HEX);
});

test("a picked accent survives a restart", () => {
  const { backend, data } = memoryStore();
  persistAccent(backend, "#BDA3E0");
  assert.equal(data.get(ACCENT_STORE_KEY), "#bda3e0");
  assert.equal(loadBootAccent(backend), LILAC);
});

test("persisting never throws and never stores garbage", async () => {
  const { backend, data } = memoryStore();
  persistAccent(backend, "not a colour");
  assert.equal(data.size, 0);
  persistAccent(null, LILAC);
  assert.doesNotThrow(() =>
    persistAccent(
      {
        getItem: () => null,
        setItem: () => {
          throw new Error("disk full");
        },
      },
      LILAC,
    ),
  );
  // An async store that rejects must not surface as an unhandled rejection.
  persistAccent({ getItem: () => null, setItem: () => Promise.reject(new Error("x")) }, LILAC);
  await new Promise((r) => setTimeout(r, 0));
});

test("every preset round-trips through the store", () => {
  for (const preset of ACCENT_PRESETS) {
    const { backend } = memoryStore();
    persistAccent(backend, preset.c);
    assert.equal(loadBootAccent(backend), normalizeAccentHex(preset.c));
  }
});

test("the synced preference: a saved accent always wins once loaded", () => {
  assert.equal(accentFromPreferences(undefined, true), null);
  assert.equal(accentFromPreferences(null, true), null);
  assert.equal(accentFromPreferences({ accent_hex: "#BDA3E0" }, false), "#bda3e0");
  assert.equal(accentFromPreferences({ accent_hex: "#BDA3E0" }, true), "#bda3e0");
});

test("the pre-unlock {} never undoes the boot accent", () => {
  // Before the PIN the read fails and answers {}.
  assert.equal(accentFromPreferences({}, false), null);
  assert.equal(accentFromPreferences({ accent_hex: 42 }, false), null);
});

test("a post-unlock read with no accent means the default", () => {
  // A second account (or an iOS reinstall) must not keep this colour.
  assert.equal(accentFromPreferences({}, true), DEFAULT_ACCENT_HEX);
  assert.equal(accentFromPreferences({ accent_hex: "nope" }, true), DEFAULT_ACCENT_HEX);
});

test("sign-out returns to the default; a cold start or sign-in does not", () => {
  assert.equal(accentOnUserChange("u1", null), DEFAULT_ACCENT_HEX);
  assert.equal(accentOnUserChange(null, "u1"), null);
  assert.equal(accentOnUserChange(null, null), null);
  assert.equal(accentOnUserChange("u1", "u1"), null);
});

const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

test("theme/tokens.ts seeds the accent from the device store while it loads", () => {
  const src = read("theme/tokens.ts");
  assert.match(src, /const _bootAccentHex = loadBootAccent\(deviceAccentBackend\);/);
  assert.match(src, /let _accentRgb = hexToRgbTriplet\(_bootAccentHex\);/);
  const device = read("theme/deviceAccent.ts");
  assert.match(device, /SecureStore\.getItem\(/);
  assert.doesNotMatch(device, /getItemAsync/);
});

test("ThemeProvider starts from the live tokens and persists every change", () => {
  const src = read("components/theme.tsx");
  assert.match(src, /useState\(currentAccentHex\)/);
  assert.doesNotMatch(src, /useState\(DEFAULT_ACCENT_HEX\)/);
  assert.match(src, /persistAccent\(deviceAccentBackend, hex\)/);
  assert.match(src, /accentFromPreferences\(prefs, trusted\)/);
  assert.match(src, /if \(unlocked\) \{[\s\S]*?refetch\(\)/);
  assert.match(src, /accentOnUserChange\(/);
});

function walk(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) {
      walk(full, out);
    } else if (/\.tsx?$/.test(full)) {
      out.push(full);
    }
  }
  return out;
}

const sources = ["app", "components", "hooks", "lib"].flatMap((d) =>
  walk(join(ROOT, d)).map((f) => ({
    rel: relative(ROOT, f),
    sf: ts.createSourceFile(f, readFileSync(f, "utf8"), ts.ScriptTarget.Latest, true),
  })),
);

const SUBSCRIBES = /\buseTheme(Rerender)?\(/;

function findFunction(sf: ts.SourceFile, name: string): ts.FunctionLikeDeclaration | null {
  let found: ts.FunctionLikeDeclaration | null = null;
  const visit = (n: ts.Node) => {
    if (found) {
      return;
    }
    if (ts.isFunctionDeclaration(n) && n.name?.text === name) {
      found = n;
    } else if (
      ts.isVariableDeclaration(n) &&
      ts.isIdentifier(n.name) &&
      n.name.text === name &&
      n.initializer &&
      (ts.isArrowFunction(n.initializer) || ts.isFunctionExpression(n.initializer))
    ) {
      found = n.initializer;
    }
    ts.forEachChild(n, visit);
  };
  visit(sf);
  return found;
}

// The component a wrapper call like observer(X) / memo(function X() {}) wraps.
function wrapped(sf: ts.SourceFile, arg: ts.Expression): ts.FunctionLikeDeclaration | null {
  if (ts.isIdentifier(arg)) {
    return findFunction(sf, arg.text);
  }
  if (ts.isArrowFunction(arg) || ts.isFunctionExpression(arg)) {
    return arg;
  }
  return null;
}

function routeComponent(sf: ts.SourceFile): ts.FunctionLikeDeclaration | null {
  for (const s of sf.statements) {
    if (
      ts.isFunctionDeclaration(s) &&
      s.modifiers?.some((m) => m.kind === ts.SyntaxKind.DefaultKeyword)
    ) {
      return s;
    }
    if (ts.isExportAssignment(s)) {
      const e = s.expression;
      return ts.isCallExpression(e) ? wrapped(sf, e.arguments[0]) : wrapped(sf, e);
    }
  }
  return null;
}

const importsTokens = (sf: ts.SourceFile) =>
  sf.statements.some(
    (s) =>
      ts.isImportDeclaration(s) &&
      ts.isStringLiteral(s.moduleSpecifier) &&
      /theme\/tokens$/.test(s.moduleSpecifier.text),
  );

test("every route component re-renders on a palette change", () => {
  for (const { rel, sf } of sources) {
    if (!rel.startsWith("app/") || !rel.endsWith(".tsx") || basename(rel) === "_layout.tsx") {
      continue;
    }
    const fn = routeComponent(sf);
    assert.ok(fn?.body, `${rel}: no default-exported component found`);
    assert.match(fn!.body!.getText(), SUBSCRIBES, `${rel}: call useThemeRerender() in the route component`);
  }
});

test("every memoised component that reads tokens re-renders on a palette change", () => {
  for (const { rel, sf } of sources) {
    if (!importsTokens(sf)) {
      continue;
    }
    const visit = (n: ts.Node) => {
      if (
        ts.isCallExpression(n) &&
        ts.isIdentifier(n.expression) &&
        /^(observer|memo)$/.test(n.expression.text) &&
        n.arguments.length > 0
      ) {
        const fn = wrapped(sf, n.arguments[0]);
        assert.ok(fn?.body, `${rel}: cannot resolve the component inside ${n.getText().slice(0, 40)}`);
        assert.match(
          fn!.body!.getText(),
          SUBSCRIBES,
          `${rel}: ${n.expression.text}() skips parent re-renders; call useThemeRerender() inside`,
        );
      }
      ts.forEachChild(n, visit);
    };
    visit(sf);
  }
});

// `const name = useCallback(...)` / `useMemo(...)`: the same function across
// renders, so the list never re-renders its rows on its own.
function isMemoisedCallback(sf: ts.SourceFile, name: string): boolean {
  let memo = false;
  const visit = (n: ts.Node) => {
    if (
      ts.isVariableDeclaration(n) &&
      ts.isIdentifier(n.name) &&
      n.name.text === name &&
      n.initializer &&
      ts.isCallExpression(n.initializer) &&
      /^(useCallback|useMemo)$/.test(n.initializer.expression.getText())
    ) {
      memo = true;
    }
    ts.forEachChild(n, visit);
  };
  visit(sf);
  return memo;
}

test("a list with a memoised renderItem passes the accent as extraData", () => {
  for (const { rel, sf } of sources) {
    const visit = (n: ts.Node) => {
      if (
        (ts.isJsxSelfClosingElement(n) || ts.isJsxOpeningElement(n)) &&
        /(^|\.)(FlatList|SectionList)$/.test(n.tagName.getText())
      ) {
        const attrs = n.attributes.properties.filter(ts.isJsxAttribute);
        const get = (name: string) => attrs.find((a) => a.name.getText() === name);
        const render = get("renderItem");
        const expr =
          render?.initializer && ts.isJsxExpression(render.initializer)
            ? render.initializer.expression
            : undefined;
        if (expr && ts.isIdentifier(expr) && isMemoisedCallback(sf, expr.text)) {
          assert.match(
            get("extraData")?.getText() ?? "",
            /accentHex/,
            `${rel}: ${n.tagName.getText()} renderItem={${expr.text}} needs extraData={accentHex}`,
          );
        }
      }
      ts.forEachChild(n, visit);
    };
    visit(sf);
  }
});

test("no colour is captured at module scope", () => {
  const TOKENS = new Set(["semantic", "palette", "type"]);
  const CALLS = new Set(["t", "tOpaque", "currentTheme", "currentAccentHex"]);
  for (const { rel, sf } of sources) {
    if (!importsTokens(sf)) {
      continue;
    }
    const imported = new Set<string>();
    for (const s of sf.statements) {
      if (
        ts.isImportDeclaration(s) &&
        /theme\/tokens$/.test((s.moduleSpecifier as ts.StringLiteral).text) &&
        s.importClause?.namedBindings &&
        ts.isNamedImports(s.importClause.namedBindings)
      ) {
        for (const el of s.importClause.namedBindings.elements) {
          // `type as ty` → the local name is ty, the token is `type`.
          if (TOKENS.has((el.propertyName ?? el.name).text) || CALLS.has((el.propertyName ?? el.name).text)) {
            imported.add(el.name.text);
          }
        }
      }
    }
    const visit = (n: ts.Node) => {
      if (ts.isFunctionLike(n) || ts.isClassLike(n)) {
        return;
      }
      const hit =
        (ts.isPropertyAccessExpression(n) &&
          ts.isIdentifier(n.expression) &&
          imported.has(n.expression.text)) ||
        (ts.isCallExpression(n) && ts.isIdentifier(n.expression) && imported.has(n.expression.text));
      if (hit) {
        const { line } = sf.getLineAndCharacterOfPosition(n.getStart());
        assert.fail(
          `${rel}:${line + 1}: ${n.getText()} is read once at import and never follows the palette; read it at render`,
        );
      }
      ts.forEachChild(n, visit);
    };
    for (const s of sf.statements) {
      if (!ts.isImportDeclaration(s)) {
        visit(s);
      }
    }
  }
});
