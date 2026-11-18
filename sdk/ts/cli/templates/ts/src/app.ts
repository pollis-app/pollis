import { app } from "@pollis-inc/app";

export default app({
  manifest: {
    // Where the App works. "*" means: whichever channels the admin picks
    // when approving it. Use "#name" to ask for one channel by name.
    read: { channels: ["*"] },
    // What the App may do there. The admin sees this list before approving.
    actions: ["reply"],
    // No `egress`: this App cannot send anything outside the group.
    // The slash commands members can pick in the composer.
    commands: [{ name: "hello", description: "Say hello", arg_hint: "[name]" }],
  },

  // Runs when a member sends one of the commands above, e.g. "/hello Sam".
  async onCommand(cmd, ctx) {
    if (cmd.name !== "hello") {
      return;
    }
    const who = cmd.args.trim() || cmd.message.author.displayName;
    await ctx.reply(cmd.message, `Hello, ${who}!`);
  },
});
