# Bots: using them in a group

For everyone in a group that has bots. You don't need to be an admin, and you don't need to install anything.

## Contents

- [Recognising a bot](#recognising-a-bot)
- [Slash commands](#slash-commands)
- [When two Apps have the same command](#when-two-apps-have-the-same-command)
- [Mentioning a bot](#mentioning-a-bot)
- [When the bot is offline](#when-the-bot-is-offline)
- [What the bot sees](#what-the-bot-sees)

## Recognising a bot

A bot is a member of the group with a **Bot** badge beside its name: in the member list, on its messages, and in the list that opens when you type `@`. Each bot runs an **App**, the code that decides what it does, and belongs to this group only. The group's admins chose it and decided what it may read and do.

## Slash commands

Many Apps answer **commands**. To use one:

1. In a channel, type `/` at the start of the message box.
2. A list opens with the commands you can use in this channel: each shows its name, what to type after it (greyed), and a one-line description. Keep typing to narrow the list.
3. Pick one with the arrow keys and Enter (or Tab), or click or tap it. The command fills the message box.
4. Type anything the command needs after it, and send.

For example, with an issue-tracker App:

```
/bug Printer on floor 3 is jammed
```

The bot answers in the channel, like any other member. The list only shows commands of Apps that the admins allowed in the channel you are in, so a command missing from the list here may work in another channel.

A command is an ordinary message: everyone in the channel sees what you sent, and the App's reply. Members on an older version of Pollis see it as a normal message too.

## When two Apps have the same command

Two Apps in a group can both have, say, `/ticket`. The list then shows each with the App's name beside it:

```
/ticket <title>   File a Linear issue     · linear  Bot
/ticket <title>   File a Jira issue       · jira    Bot
```

When you pick one, the message box fills with the command **and that bot's name**, for example `/ticket @linear_bot `. That is how Pollis knows which App you meant: only the bot named after the command answers. If you type a shared command yourself without naming a bot, Pollis doesn't send it and asks you to pick one from the list.

## Mentioning a bot

Type `@` and the start of the bot's name; bots are in the list with the Bot badge. A message that mentions a bot reaches that bot only, if its App listens for mentions. What it does with the message is up to the App.

## When the bot is offline

The bot runs on a machine one of the admins controls. If that machine is off, your command still sends normally and shows as sent. The bot handles it, in order, when its machine is back. There is nothing to wait for and nothing to resend.

## What the bot sees

A bot is a full member of the group, so it can decrypt messages like any member. Its App only ever receives messages from the channels the admins allowed it, and it can only act in ways the admins allowed. The [admin guide](./admin-guide.md#what-a-bot-can-read) explains what a bot can read and who decides.
