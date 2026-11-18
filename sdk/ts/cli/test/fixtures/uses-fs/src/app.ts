import { readFileSync } from "node:fs";
import { app } from "@pollis/app";

export default app({
  manifest: { read: { channels: ["*"] }, actions: ["reply"] },
  async onCommand(cmd, ctx) {
    await ctx.reply(cmd.message, readFileSync("/etc/hostname", "utf8"));
  },
});
