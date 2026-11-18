import { test } from "node:test";
import assert from "node:assert/strict";

import { ManifestError, nameFromPackage, resolveManifest } from "../dist/manifest.js";
import { app, isApp } from "../dist/index.js";

const pkg = { name: "@acme/Linear_Bot", version: "2.0.0", description: "Files tickets." };

test("defaults come from package.json and the handlers", () => {
  const m = resolveManifest({ read: { channels: ["#support"] }, actions: ["reply"] }, ["onCommand", "onMessage"], pkg);
  assert.deepEqual(m, {
    name: "linear-bot",
    version: "2.0.0",
    description: "Files tickets.",
    triggers: ["messages", "commands"],
    read: { channels: ["#support"], history: 0, events: ["created"] },
    actions: ["reply"],
    state: { quota: 0 },
    egress: [],
  });
});

test("declared triggers must match the handlers", () => {
  assert.throws(() => resolveManifest({ triggers: ["commands"] }, [], pkg), /no `onCommand` handler/);
  assert.throws(() => resolveManifest({ triggers: ["commands"] }, ["onCommand", "onMessage"], pkg), /`onMessage` would never run/);
  assert.throws(() => resolveManifest({}, [], pkg), /no handlers/);
});

test("egress is exact host names only", () => {
  for (const bad of ["https://api.x.com", "*.x.com", "api.x.com:443", "10.0.0.1", "API.X.COM", "localhost"]) {
    assert.throws(() => resolveManifest({ egress: [bad] }, ["onCommand"], pkg), ManifestError, bad);
  }
  assert.deepEqual(resolveManifest({ egress: ["api.linear.app"] }, ["onCommand"], pkg).egress, ["api.linear.app"]);
});

test("unknown fields and values are named", () => {
  assert.throws(() => resolveManifest({ egres: [] } as never, ["onCommand"], pkg), /`egres` is not a manifest field/);
  assert.throws(() => resolveManifest({ actions: ["ban"] } as never, ["onCommand"], pkg), /`actions` has "ban"/);
  assert.throws(() => resolveManifest({ read: { channels: ["#a b"] } }, ["onCommand"], pkg), /`read.channels` entry/);
  assert.throws(() => resolveManifest({ read: { history: -1 } }, ["onCommand"], pkg), /`read.history` must be a whole number/);
  assert.throws(() => resolveManifest({ name: "Bad Name" }, ["onCommand"], pkg), /`name` "Bad Name" is not valid/);
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
