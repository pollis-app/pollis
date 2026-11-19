# The alerts App

Posts the alerts your monitoring, CI or scripts send to a bot's webhook into a channel of your group.

A sender posts `{"channel": "alerts", "text": "disk 91% on db-1"}` and the bot posts that text in `#alerts`. The App asks for one thing: to post, from its webhook, in the channels an admin picks. It reads no messages, keeps no state and cannot send anything outside the group.

It is published the way any App should be: this directory is its source, [`APP_HASH`](./APP_HASH) is the hash of its official build, and the manifest is written by the build to `dist/pollis-app.json`. Being made by Pollis gives it no extra permission. An admin approves it like any other App.

## Build it and check the hash

You need [Rust through rustup](https://rustup.rs), Node.js 22.18 or newer, pnpm, and a checkout of this repository.

```sh
# Once: the tools
cargo build --release -p pollis-app-dev
export PATH="$PWD/target/release:$PATH"
(cd sdk/ts && pnpm install && pnpm build)

# The App
node sdk/ts/cli/dist/main.js app build --dir apps/alerts
cat apps/alerts/APP_HASH
```

The `App hash:` line the build prints must equal `APP_HASH`. `Cargo.lock` and `rust-toolchain.toml` pin every dependency and the compiler, so the same commit gives the same hash on any machine, in any directory. The build leaves `dist/app.wasm` and `dist/pollis-app.json`.

Run its tests the same way:

```sh
node sdk/ts/cli/dist/main.js app test --dir apps/alerts
```

## Set up an alert bot

1. Pair a bot with your group ([Self-hosting](../../docs/bots/self-hosting.md)). `pollis-bot status` shows its installation id.
2. On the bot's machine, give it this App. `app install` prints the App hash; check it against `APP_HASH`:

   ```sh
   pollis-bot app install --installation <installation id> --artifact apps/alerts/dist
   ```

3. Start the bot with its webhook on, and keep the secret it prints once:

   ```sh
   pollis-bot run --webhook
   ```

4. A group admin opens **Bots**, then the bot, checks the App hash, picks the channels it may post in, and approves ([Approving an App](../../docs/bots/app-approval.md)).
5. Send an alert ([Alerts webhook](../../docs/bots/alerts-webhook.md)):

   ```sh
   pollis-bot post --installation <installation id> --channel alerts "backup finished"
   ```

A channel created after the approval is not included until an admin adds it on the bot's page.

## Changing it

Any change to this App or to the SDK it builds with gives a new App hash. Rebuild, then update the published hash and the copy the runtime's tests run:

```sh
node sdk/ts/cli/dist/main.js app build --dir apps/alerts
pollis-app-dev hash apps/alerts/dist/app.wasm apps/alerts/dist/pollis-app.json > apps/alerts/APP_HASH
cp apps/alerts/dist/app.wasm apps/alerts/dist/pollis-app.json pollis-bot/tests/fixtures/alerts-app/
```
