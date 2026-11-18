# Bots: approving an App

For group admins, and for developers who want to know what an admin sees. A bot that runs an **App** runs exactly the App an admin approved, with exactly the permissions the admin chose, and nothing else. This page explains how that works and how to do it.

Everything here works the same in the desktop app and the mobile app: open the group's **Bots** page, then the bot.

## Contents

- [The short version](#the-short-version)
- [Approve an App](#approve-an-app)
- [What the App can do, in plain language](#what-the-app-can-do-in-plain-language)
- [Narrow what it can do](#narrow-what-it-can-do)
- [Updates](#updates)
- [Updates that approve themselves](#updates-that-approve-themselves)
- [Development installations](#development-installations)
- [When a bot stops running](#when-a-bot-stops-running)
- [For developers: what your users approve](#for-developers-what-your-users-approve)
- [How it is checked](#how-it-is-checked)

## The short version

- Every App has an **App hash**: a fingerprint of its code and its manifest together. Change one byte of either and the hash changes.
- An admin approves one App hash, with the permissions (grants) they chose. The approval is signed by the admin's own device.
- The bot's runtime checks that signature, that the signer is still an admin, and that the App it is about to run has exactly the approved hash, **before every job**. If anything does not match, the App does not run and the group's admins see why on the bot's page.
- A new version needs a new approval, unless you let updates that ask for nothing more run by themselves. If it asks for more than before, the extra access is highlighted and is never switched on for you.

## Approve an App

Whoever runs the bot's machine points the bot at an App, from a file on that machine or an `https://` address ([Self-hosting](./self-hosting.md)). The bot cannot run it yet. On the bot's page you see:

- **Waiting for your approval**, with the App's name, version and description;
- its **App hash** (compare it with the hash the developer published, or with what `pollis app build` printed if you built it yourself);
- where it comes from, exactly as it was given on the machine;
- what it can do, in plain language (next section), with switches to narrow it.

Check it, narrow anything you do not want, and choose **Approve**. The bot starts the App at its next message or webhook call.

Only admins approve. Every admin sees the same page and can change the approval later.

## What the App can do, in plain language

The page lists what the App asked for in its manifest, one line each, for example:

- **Reads #alerts and #ops, last 30 days**: the channels it receives messages from and how far back it may look.
- **Wakes on new messages, mentions and commands**.
- **Can reply and react**.
- **Can send data to: api.linear.app**: the only internet addresses it may reach. An App sees what the bot reads, so it can send what it reads to these addresses.
- **Can't send data outside this group**: shown instead when the App may reach no address at all.
- **Keeps up to 4 KB of its own data**, sealed so only the bot's machines can read it.

## Narrow what it can do

On the approve page, and at any time afterwards:

- switch off channels it should not read;
- shorten how far back it may read;
- switch off internet addresses it should not reach.

You can only take things away. Nothing can be added beyond what the App's manifest asked for. Saving signs a new approval for the same App with the narrower permissions.

## Updates

When the machine has a new version of the App (a new file, or new content at the same address), the bot stops running the old approval and waits. The bot's page shows **Waiting for your approval** with a list of what changed:

- **New access** is highlighted at the top: a new channel, a longer history, a new kind of message it reads, a new action, a new internet address, a larger data allowance, a new way to wake it, a new slash command, or a new or more frequent schedule. Each one has its own switch, **off** until you switch it on. Approving without switching it on approves the update without that access. A new way to wake it, a new command, a new schedule and a larger data allowance come with the version itself, so the update can only be approved once you switch them on.
- Everything else (things it no longer asks for) is listed below.

If nothing in the update asks for more, **Approve update** takes one tap and keeps your current choices.

## Updates that approve themselves

When approving, you can switch on **Approve updates automatically**. Your signed approval then also says: "a newer version from this same address may run, as long as it asks for nothing more". The bot itself checks that before every job, so an update lands even when no admin is online:

- a new version from exactly the approved address that asks for nothing more runs, with your choices carried over (anything it no longer asks for is dropped);
- a version that asks for more, or one from anywhere else, waits for an admin as usual;
- once the bot runs a newer version it never goes back to an older one under the same approval, even if the older file is served again.

The bot's page shows which version is running and that it updated itself. Every admin sees that the setting is on.

## Development installations

A developer working on an App rebuilds it many times an hour; approving each build would be unusable. For a test group, switch on **Development installation** on the bot's page. Instead of one App hash, you approve the developer's **signing key** (shown as a short fingerprint):

- the bot runs any build signed by that key, as long as the build asks for **no more** than what you approved;
- a build that asks for more is refused until you approve it;
- every member of the group sees a **Development build** mark on the bot for as long as the setting is on.

Use it in test groups. To leave development, approve the current build as a normal App (switch it off and approve).

## When a bot stops running

If the bot cannot check its approval, it does not run the App, and its page shows why:

| What you see | What happened | What to do |
|---|---|---|
| Waiting for your approval | No approval yet, or the machine has a different version | Check it and approve |
| The approving admin is no longer an admin | The admin who approved was removed or is no longer an admin | Any admin approves again |
| The approval's signature does not check out | The approval was changed after it was signed | Approve again; if it repeats, tell whoever runs your Pollis server |
| An older approval was served | The server sent an approval older than one the bot already accepted | The bot keeps refusing it; approve again |
| Can't load the App | The file or address given on the machine cannot be read | Whoever runs the machine fixes the path or address |

Messages that arrive while the App is not running are not handed to it later.

## For developers: what your users approve

- **Ask for the least you need** in `pollis-app.json` ([App reference → the manifest](./app-reference.md#the-manifest-pollis-appjson)). Admins see every field in plain language, can take any of it away, and see every widening highlighted when you ship an update.
- **The App hash** is `SHA-256("pollis-app-v1" ‖ len(component) ‖ component ‖ JCS(manifest))`, with `len` an unsigned 64-bit big-endian integer and JCS the JSON Canonicalization Scheme (RFC 8785). Reformatting the manifest does not change it; any other change does. `pollis app build` prints it; publish it beside your release so admins can compare.
- **Your App never sees its grants change mid-job.** The runtime checks the approval at the start of each job and runs that job with the approved permissions; anything outside them is refused, as described in the [App reference](./app-reference.md#effective-grants).
- **Development builds.** `pollis-bot app dev-key` creates your developer key and prints its public half; `pollis-bot app sign --artifact <dir>` signs a build (it writes `<component>.sig.json` beside the component). An admin approves the key once, as a development installation; after that every signed build that asks for no more runs without a new approval.

## How it is checked

Every approval is a small record signed by the approving admin's device: which bot and group, a sequence number, the App hash (or, for development, the developer key), the manifest's own hash, the permissions, who approved and when. The Pollis server stores it but cannot make or change one, because it has no admin's key.

Before every job the bot's runtime checks, from its own copy of the group:

1. the signature, under the key of the admin's device, and that device's certificate under the admin's account key (and the public key log);
2. that the admin is still an admin, in both the group's encrypted membership and the member list;
3. that the App it is about to run hashes to the approved App hash, and the manifest to the approved manifest hash;
4. that the permissions ask for nothing the manifest did not;
5. that the approval is not older than the newest one it already accepted, so the server cannot quietly bring back an older version.

The full model, with the tests that pin each rule, is in the [threat model](../bots-threat-model.md#app-approval).
