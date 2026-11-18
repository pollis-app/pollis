# pollis:app — the App capability interface

The contract between a Pollis App and the bot runtime (#2004). SDKs generate from here.

- `wit/app.wit` — WIT package `pollis:app@0.1.0`: the `app` world (exported handlers, imported `host` functions) and its data types.
- `pollis-app.schema.json` — JSON schema for `pollis-app.json`, the manifest shipped beside the component.

The runtime side is `pollis-app-host` (sandbox and enforcement) and `pollis-app-manifest` (manifest and grants). Never edit a published version in place; a breaking change is a new package version.
