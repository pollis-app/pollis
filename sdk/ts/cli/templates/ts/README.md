# {{name}}

A [Pollis](https://pollis.com) App. It answers `/hello` with a greeting.

```sh
pnpm install
pnpm test            # build, then run src/app.test.ts against the built App, offline
pnpm build           # writes dist/app.wasm and dist/pollis-app.json, prints the App hash
```

- `src/app.ts`: the App. Its `manifest` says what it needs; its handlers say what it does.
- `src/app.test.ts`: tests. Each one runs the built App in the same sandbox the bot
  runtime uses, so anything the manifest does not allow fails here too.
- `dist/`: build output. The App hash identifies exactly this build; an admin approves it.

The developer guide explains every handler, what `ctx` can do, the manifest and the
sandbox's limits: https://github.com/pollis-app/pollis/blob/main/docs/bots/developer-guide.md
