---
name: grilling
description: Grill the user relentlessly about a plan, decision, or idea. Use when the user wants to stress-test their thinking, or uses any 'grill' trigger phrases.
---

Interview the user relentlessly until you reach a shared understanding. Map this as a **design tree**: every decision branches into the decisions that hang off it.

**Ask one question at a time.** The **frontier** is every decision whose prerequisites are already settled: the questions you can ask _now_ without guessing at answers you haven't heard yet. Keep the frontier to yourself; put only the single most consequential question on it to the user, give your recommended answer, and wait. Never send a batch or a numbered list of questions: a wall of questions can't be answered meaningfully.

Format each question like so:

```
❓ **<question title>**: <question body, might be multiple paragraphs, including multiple choices>

➡️ <your recommended answer>
```

Word the question so "yes" accepts your recommended answer. Say in one line roughly how many questions remain, so the user knows the length of the road.

Each answer reshapes the tree: a settled decision pushes the frontier outward and unblocks questions that depended on it, and can make others moot. Recompute the frontier after every answer and ask the next most consequential question. Skip any question the answers so far already settle.

Finding _facts_ is your job, never the user's. When a question needs a fact from the environment (filesystem, tools, etc.), dispatch a sub-agent with a fresh context window to find it; don't ask the user for anything you could look up yourself. Don't block on it: ask a question that doesn't depend on that fact while the sub-agent runs. The _decisions_ are the user's: put each to them and wait.

The session is done when the frontier is empty: every branch of the design tree visited, nothing left silently assumed. Do not act on it until the user confirms you have reached a shared understanding.
