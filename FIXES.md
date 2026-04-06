# Pending fixes (session 2026-04-05)

- Members list: add 2rem top padding so first row isn't flush against the header border
- InviteMember, SearchGroup: wrap in `<form>` so pressing Enter fires the submit button
- Logout: re-fetch known accounts after signing out so "Previously signed in" list appears on the auth screen
- Groups page: show pending join-request count badge on each group row (admin only)
- New `/join-requests` route: aggregated view of all pending join requests across groups where user is admin
- Root menu "Join Requests": navigate to `/join-requests` instead of `/groups`
- Root menu DMs description: changed from live conversation count to static descriptive copy
- Replies: `reply_to_id` not rendering decorator on sent messages; fix pass-through to invoke + render
- Replies: chat input does not auto-focus when user clicks Reply
- Known account chips: made clickable — stores email in accounts index on login, chips auto-trigger OTP and skip to code-entry step
- Message history disappearing: watermark was storing fetch time (datetime('now')) instead of latest message sent_at, causing all envelopes to be deleted the moment both users had fetched once; fixed in get_channel_messages and get_dm_messages
- Message history disappearing (follow-up): stale wall-clock watermarks in the live DB still triggered premature deletion; added 30-day retention floor to both cleanup DELETE queries so envelopes are never removed until 30 days old regardless of watermark state
