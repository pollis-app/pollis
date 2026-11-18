import { test } from "node:test";
import assert from "node:assert/strict";

import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { ManifestError, nameFromPackage, resolveManifest, validateManifest } from "../dist/manifest.js";
import { app, isApp } from "../dist/index.js";

const pkg = { name: "@acme/Linear_Bot", version: "2.0.0", description: "Files tickets." };

test("defaults come from package.json and the handlers", () => {
  const m = resolveManifest(
    { read: { channels: ["#support"] }, actions: ["reply"], commands: [{ name: "bug", description: "File a bug", arg_hint: "<title>" }] },
    ["onCommand", "onMessage"],
    pkg,
  );
  assert.deepEqual(m, {
    name: "linear-bot",
    version: "2.0.0",
    description: "Files tickets.",
    triggers: ["messages", "commands"],
    read: { channels: ["#support"], history: 0, events: ["created"] },
    actions: ["reply"],
    state: { quota: 0 },
    egress: [],
    commands: [{ name: "bug", description: "File a bug", arg_hint: "<title>" }],
  });
});

test("declared triggers must match the handlers", () => {
  assert.throws(() => resolveManifest({ triggers: ["commands"] }, [], pkg), /no `onCommand` handler/);
  const commands = [{ name: "x", description: "X" }];
  assert.throws(() => resolveManifest({ triggers: ["commands"], commands }, ["onCommand", "onMessage"], pkg), /`onMessage` would never run/);
  assert.throws(() => resolveManifest({}, ["onCommand"], pkg), /`onCommand` needs its commands declared/);
  assert.throws(() => resolveManifest({}, ["onSchedule"], pkg), /`onSchedule` needs `schedules`/);
  assert.throws(() => resolveManifest({}, [], pkg), /no handlers/);
});

test("egress is exact host names only", () => {
  for (const bad of ["https://api.x.com", "*.x.com", "api.x.com:443", "10.0.0.1", "API.X.COM", "localhost"]) {
    assert.throws(() => resolveManifest({ egress: [bad] }, ["onMention"], pkg), ManifestError, bad);
  }
  assert.deepEqual(resolveManifest({ egress: ["api.linear.app"] }, ["onMention"], pkg).egress, ["api.linear.app"]);
});

test("unknown fields and values are named", () => {
  assert.throws(() => resolveManifest({ egres: [] } as never, ["onMention"], pkg), /`egres` is not a manifest field/);
  assert.throws(() => resolveManifest({ actions: ["ban"] } as never, ["onMention"], pkg), /`actions` has "ban"/);
  assert.throws(() => resolveManifest({ read: { channels: ["#a b"] } }, ["onMention"], pkg), /`read.channels` entry/);
  assert.throws(() => resolveManifest({ read: { history: -1 } }, ["onMention"], pkg), /`read.history` must be a whole number/);
  assert.throws(() => resolveManifest({ name: "Bad Name" }, ["onMention"], pkg), /`name` "Bad Name" is not valid/);
});

test("package names become App names", () => {
  assert.equal(nameFromPackage("@scope/My_App"), "my-app");
  assert.equal(nameFromPackage("hello"), "hello");
  assert.equal(nameFromPackage(undefined), undefined);
});

test("app() marks a definition", () => {
  const def = app({ manifest: {}, onCommand() {} });
  assert.ok(isApp(def));
  assert.ok(!isApp({ manifest: {} }));
  assert.throws(() => app(null as never), TypeError);
});

// The same fixtures pollis-app-dev/tests/manifest_fixtures.rs holds the
// runtime's parser to: the SDK accepts and refuses exactly what it does.
const fixtures = join(dirname(fileURLToPath(import.meta.url)), "fixtures", "manifests");
for (const kind of ["valid", "invalid"]) {
  for (const file of readdirSync(join(fixtures, kind)).sort()) {
    test(`${kind} manifest fixture ${file}`, () => {
      const json = JSON.parse(readFileSync(join(fixtures, kind, file), "utf8"));
      if (kind === "valid") {
        validateManifest(json);
      } else {
        assert.throws(() => validateManifest(json), ManifestError);
      }
    });
  }
}

test("emitted commands keep snake_case arg_hint and drop a false request", () => {
  const m = validateManifest({
    name: "a",
    version: "1",
    triggers: ["commands"],
    commands: [{ name: "bug", description: "File", arg_hint: "<t>", request: false }],
  });
  assert.deepEqual(m.commands, [{ name: "bug", description: "File", arg_hint: "<t>" }]);
  assert.equal(m.schedules, undefined);
});
