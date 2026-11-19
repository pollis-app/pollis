# {{name}}

A [Pollis](https://pollis.com) App, in Rust. It answers `/hello` with a greeting.

```sh
npx pollis app test     # build, then run tests/hello.rs against the built App, offline
npx pollis app build    # writes dist/app.wasm and dist/pollis-app.json, prints the App hash
```

- `src/lib.rs`: the App. Its `manifest` says what it needs; its handlers say what it does.
- `tests/hello.rs`: tests. Each one runs the built App in the same sandbox the bot
  runtime uses, so anything the manifest does not allow fails here too.
- `Cargo.lock` and `rust-toolchain.toml`: commit both. With them, anyone can rebuild this
  App from source and get the same App hash.
- `dist/`: build output. The App hash identifies exactly this build; an admin approves it.

The developer guide explains every handler, what `ctx` can do, the manifest and the
sandbox's limits: https://github.com/pollis-app/pollis/blob/main/docs/bots/developer-guide.md
