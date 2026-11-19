# Bots: self-hosting

For whoever runs the machine a bot lives on. You will install `pollis-bot`, pair it with a group, and keep it running.

The full command reference is `pollis-bot --help` and [`pollis-bot/README.md`](../../pollis-bot/README.md).

## Contents

- [What you need](#what-you-need)
- [Install](#install)
- [Pair](#pair)
- [Run](#run)
- [Run an App](#run-an-app)
- [Run as a service](#run-as-a-service)
- [How the keys are kept](#how-the-keys-are-kept)
- [Several bots on one machine](#several-bots-on-one-machine)
- [Add this machine as another host](#add-this-machine-as-another-host)
- [Upgrade](#upgrade)
- [Files](#files)
- [Logs and status](#logs-and-status)
- [Every failure and its fix](#every-failure-and-its-fix)
- [Troubleshooting](#troubleshooting)

## What you need

- A machine that stays on: a home server, a VM, a Raspberry Pi-class Linux box, a Mac. Linux, macOS and Windows all work.
- Outbound internet access to the Pollis Delivery Service. The runtime opens no port of its own, except the optional webhook.
- A group admin who can type the pairing code into Pollis ([Admin guide](./admin-guide.md#add-a-bot)).

A bot reads every message in its group. Use a machine you would trust with that.

## Install

`pollis-bot` is built from this repository.

1. Install Rust (stable) from <https://rustup.rs>.
2. Build and install the binary:

   ```bash
   git clone https://github.com/pollis-app/pollis.git
   cd pollis
   cargo build --release -p pollis-bot
   install -m 755 target/release/pollis-bot ~/.local/bin/
   ```

3. Tell it which Delivery Service to use. These are the same variables the `pollis` terminal client reads; `R2_S3_ENDPOINT` and `R2_PUBLIC_URL` must be set but can be empty:

   ```bash
   export POLLIS_DELIVERY_URL=https://api.pollis.com
   export R2_S3_ENDPOINT= R2_PUBLIC_URL=
   ```

4. Check it runs:

   ```bash
   pollis-bot --version
   ```

## Pair

Pairing creates the bot and makes this machine its first host.

1. Run:

   ```bash
   pollis-bot pair --name "monitoring box"
   ```

   `--name` names this machine (the host) on the bot's page. It is optional.
2. It prints:

   ```text
       Pairing code:  7KQ2M9XD

   A group admin types this code in Pollis: open the group, then Bots → Add a bot. It expires at 14:32 UTC.
   ```

3. Give the code to a group admin. They type it on the **Add a bot** page with the bot's name.
4. When they confirm, `pair` prints `paired: bot <bot id> in group <group id>`, with the command that gives the bot an App, and exits.

The code expires ten minutes after it is printed. If it expires, run `pollis-bot pair` again.

Run `pair` as the same user, on the same machine, that will later run the service. The bot's keys are stored for that user (see [How the keys are kept](#how-the-keys-are-kept)).

## Run

```bash
pollis-bot run                 # every bot on this machine, until stopped
pollis-bot run --webhook       # also listen for webhook calls on 127.0.0.1:8787
pollis-bot status              # what each bot is doing
```

`run` opens every installation on this machine that has an App ([Run an App](#run-an-app)), catches up on anything it missed, and then waits for events. A host does not have to stay up: `pollis-bot run --once` catches up, handles everything pending (including approving another host) and exits, so a machine that sleeps can run it on a schedule or at wake. Each installation's store is unlocked only while a job runs. It stops on Ctrl-C (and on `SIGTERM` on Linux and macOS). Nothing is lost while it is stopped: the next start catches up.

To keep the webhook on without the flag, add it to `config.json` in the data directory (see [Files](#files)):

```json
{ "webhook": { "bind": "127.0.0.1:8787" } }
```

The first time the webhook is on, `run` prints each installation's webhook secret **once**. Store it where your alert sender can use it. [Alerts webhook](./alerts-webhook.md) explains the rest.

## Run an App

A bot does nothing until you give it an App: `pollis-bot run` skips it, and `pollis-bot status` says `no App yet` with the command to run. The App can be a directory on this machine (with `pollis-app.json` and one `.wasm`) or an `https://` address:

```bash
pollis-bot app install --installation <installation id> --artifact /opt/apps/triage
pollis-bot run
```

`app install` prints the App's **App hash**. The App runs only once a group admin approves that exact hash on the bot's page in Pollis, and the runtime checks the approval and the file again before every job. If the file changes (a new version), the bot stops running it until an admin approves the new one. The log says why it is not running and what to do; the admins see the same on the bot's page. See [Approving an App](./app-approval.md).

To post alerts from your monitoring or scripts, use the [alerts App](../../apps/alerts/README.md): its page shows how to build it, check its hash and set up the bot.

## Run as a service

Run `pollis-bot run` under your service manager so it starts on boot and restarts if it stops. Run it as the user who ran `pair`.

### Linux (systemd)

On a server, a system unit running as a dedicated user. Pair as that user first, with the same data directory, so the keys are stored where the service looks for them.

`/etc/systemd/system/pollis-bot.service`:

```ini
[Unit]
Description=Pollis bot runtime
Wants=network-online.target
After=network-online.target

[Service]
User=pollis-bot
Environment=HOME=/var/lib/pollis-bot
Environment=POLLIS_DELIVERY_URL=https://api.pollis.com
Environment=R2_S3_ENDPOINT=
Environment=R2_PUBLIC_URL=
ExecStart=/usr/local/bin/pollis-bot --data-dir /var/lib/pollis-bot run
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
```

```bash
sudo useradd --system --home-dir /var/lib/pollis-bot --create-home pollis-bot
sudo install -m 755 target/release/pollis-bot /usr/local/bin/
# Pair as the service user (an admin types the code it prints):
sudo -u pollis-bot -H env POLLIS_DELIVERY_URL=https://api.pollis.com R2_S3_ENDPOINT= R2_PUBLIC_URL= \
  pollis-bot --data-dir /var/lib/pollis-bot pair --name "$(hostname)"
sudo systemctl daemon-reload
sudo systemctl enable --now pollis-bot
journalctl -u pollis-bot -f       # the log
```

On a desktop Linux machine where you ran `pair` from your own session, use a user unit instead (`~/.config/systemd/user/pollis-bot.service`, the same `[Service]` section without `User=` and `HOME=`, `WantedBy=default.target`), then `systemctl --user enable --now pollis-bot`. It uses the same keychain your session does.

### macOS (launchd)

A LaunchAgent for the user who ran `pair`. It starts when that user logs in, because the macOS keychain it uses is unlocked at login.

`~/Library/LaunchAgents/com.pollis.bot.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.pollis.bot</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/pollis-bot</string>
    <string>run</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>POLLIS_DELIVERY_URL</key><string>https://api.pollis.com</string>
    <key>R2_S3_ENDPOINT</key><string></string>
    <key>R2_PUBLIC_URL</key><string></string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardErrorPath</key><string>/tmp/pollis-bot.log</string>
</dict>
</plist>
```

```bash
launchctl load ~/Library/LaunchAgents/com.pollis.bot.plist
tail -f /tmp/pollis-bot.log
```

### Windows

There is no Windows service wrapper. Start `pollis-bot run` from Task Scheduler with the trigger **At log on** for the user who ran `pair`, and **Restart if the task fails** turned on. It stops on Ctrl-C or when the task ends.

## How the keys are kept

A bot starts with nobody present, so it has no PIN. Each bot's keys and the key to its encrypted local store sit in the machine's keystore: the OS keychain where there is one, otherwise an encrypted file bound to this machine. They are never stored in plain text.

This protects against someone who copies the disk or a backup to another machine: the copy does not open there. It does not protect against someone who can run programs as the same user on this machine, or who has a full image of it. Keep the machine, and the account the bot runs as, as private as the conversations the bot can read. The security whitepaper covers the details (§3.5, "Bot hosts").

## Several bots on one machine

One runtime can host several bots, in the same group or in different groups. Each one is a separate **installation** with its own keys, its own keystore entry and its own encrypted store; nothing is shared between them.

- Run `pollis-bot pair` once per bot. Each pairing adds an installation.
- `pollis-bot run` runs all of them.
- The runtime refuses to host more than `max_installations` (default **8**), so one machine is not overloaded by accident. `pair` and `add-host` refuse to add past it, and `run` refuses to start past it. Raise it in `config.json`, or for one run with `run --max-installations N`:

  ```json
  { "max_installations": 12 }
  ```

## Add this machine as another host

To make this machine a second host of an existing bot:

1. Get the command from the bot's page in Pollis (**Add a host**). It looks like:

   ```bash
   pollis-bot add-host --bot <bot id> --name "backup box"
   ```

2. Run it here. It prints a pairing code; the admin types it on the **Add a host** page.
3. One of the bot's running hosts checks the admin's approval and hands this machine the bot's keys. At least one existing host must be running `pollis-bot run`.
4. Start this machine with `pollis-bot run`.

A machine can be one host of a given bot only once; `add-host` for a bot it already runs is refused.

## Upgrade

1. Stop the service (`sudo systemctl stop pollis-bot`, `launchctl unload …`, or end the task).
2. Rebuild and replace the binary ([Install](#install), step 2).
3. Start the service again.

Installations, keys and settings stay where they are. The first job after the start catches up on everything sent while it was stopped.

## Files

The data directory is `--data-dir`, else `POLLIS_BOT_DATA_DIR`, else:

| System | Default data directory |
|---|---|
| Linux | `$XDG_DATA_HOME/pollis-bot`, or `~/.local/share/pollis-bot` |
| macOS | `~/Library/Application Support/com.pollis.bot` |
| Windows | `%APPDATA%\pollis-bot` |

Inside it:

| Path | What |
|---|---|
| `config.json` | Optional settings: `max_installations`, and `webhook` with `bind`, `per_minute`, `burst`. Unknown fields are refused. |
| `installations/<id>/installation.json` | One installation: its bot, group and this host's device id. |
| `stores/<device id>/` | That installation's encrypted local store, including how far its job loop has read each channel. |
| `status.json` | Written by `run` whenever an installation's state changes; read by `status`. |
| `keystore.pks` | Only where the machine has no OS keychain: the keystore itself, encrypted and bound to this machine. |

The bot's keys are not in the other files; they are in the keystore (the OS keychain, or `keystore.pks`). Two runtimes on one machine need two data directories. To remove an installation from this machine (after an admin revoked the host or deleted the bot), run `pollis-bot forget --installation <id>`: it deletes the record, the store and the keys together. See also [`pollis-bot/README.md`](../../pollis-bot/README.md#files).

## Logs and status

`pollis-bot` logs to standard error, one line per event:

```text
2026-11-18T09:14:02Z pollis-bot [installation 01JD…] opening (bot 01JD… in group 01HZ…)
2026-11-18T09:14:03Z pollis-bot [webhook] listening on 127.0.0.1:8787
2026-11-18T09:15:40Z pollis-bot [installation 01JD…] webhook hook:01JD…: posted 1
```

Every failure line reads `FAILED: <what happened>. Fix: <one thing to do>`.

`pollis-bot status` shows the data directory, how many installations there are, whether the runtime is running, and each installation's state:

| State | Meaning |
|---|---|
| `starting` | The runtime has opened the installation. |
| `catching up` | It is reading what arrived since its last job. |
| `connected` | Caught up and waiting for the next message or alert. |
| `job failed` | The last job stopped. `status` shows the error and the fix; the next message, alert or restart tries again. |

During pairing the log shows `filing a pairing request`, then `waiting for approval`, then `approved; joining the bot's group`.

## Every failure and its fix

| The message says | Do this |
|---|---|
| `the code expired before an admin typed it` | Run `pair` (or `add-host`) again and have the admin type the new code within ten minutes. |
| `this host already runs bot <id>` | Run `add-host` on a different machine. |
| `this runtime is configured for at most N installations` | Raise `max_installations` in `config.json`, or run the extra bot on another machine. |
| `no installations on this host` | Run `pollis-bot pair` and have an admin type the code. |
| `opening installation <id>` | The host may have been revoked on the bot's page, or the bot deleted. If so, run `pollis-bot forget --installation <id>`, then `pair` or `add-host` again if the bot should stay. The rest of the line says why it could not open. |
| `unattended_refused: the account no longer exists` | The bot was deleted. Run `pollis-bot forget --installation <id>`. |
| `unattended_refused: no bot installation for this account on this machine` | The keystore entry is missing: `run` is running as a different user than `pair` did, or the keystore was cleared. Run as the user who paired, or pair again. |
| `installation.json is missing: remove the directory if its pairing never finished` | A pairing was interrupted. Remove that directory. |
| `config.json is not valid config` / `max_installations must be at least 1` | Fix `config.json`; `pollis-bot --help` lists its fields. |
| `binding the webhook listener to <addr>` | Another program uses that address. Pick a free one in `webhook.bind` or `run --webhook ADDR`. |
| `job failed while catching up` | Check the machine reaches the Delivery Service. The next message or a restart retries. |
| `job failed while reading the group's channels` | Check the connection, and that the bot is still in its group. |
| `job failed while claiming the trigger` | Check the connection, and that this host was not revoked (`pollis-bot status`). The alert is retried. |
| `job failed while posting` | Check the connection. The message is kept and retried under the same id, so it is never posted twice. |
| `job failed while saving progress` | Check the data directory is writable and the disk has space. |
| `` `post` to `<channel>` refused: not a text channel of this bot's group `` | Send to a text channel of the bot's group, by name or id. |
| `webhook call with a wrong secret or signature` | Give the sender this installation's current secret (`pollis-bot rotate-secret` makes a new one). |
| `writing status.json` | Check the data directory is writable. Only `status` is affected. |

## Troubleshooting

**The admin says the code doesn't match.** They should read it again from this machine's screen. Codes ignore case and spaces. If it still fails, run `pair` again for a fresh code.

**`add-host` keeps waiting after the admin typed the code.** One of the bot's existing hosts has to hand over the keys. Make sure at least one is running `pollis-bot run` and can reach the Delivery Service.

**The service starts but finds no installations.** It is using a different data directory or user than `pair` did. Use the same `--data-dir` (or `POLLIS_BOT_DATA_DIR`) and the same user for both.

**Alerts are refused with 401.** The secret is wrong, or was rotated: `rotate-secret` takes effect after `pollis-bot run` restarts.

**I lost the webhook secret.** Run `pollis-bot rotate-secret --installation <id>`, restart `pollis-bot run`, and give the new secret to your sender.

**I want to move a bot to a new machine.** Add the new machine as a host ([above](#add-this-machine-as-another-host)), check it runs, then revoke the old one on the bot's page. Copying the data directory to another machine does not work, by design.
