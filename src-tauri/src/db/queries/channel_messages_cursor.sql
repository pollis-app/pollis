-- Messages older than a cursor position (keyset / cursor-based pagination).
--
-- The cursor is the (sent_at, id) pair from the oldest row of the previous
-- page. Using both fields handles the case where two messages share the same
-- timestamp — a pure sent_at cursor would skip or repeat rows in that case.
--
-- The index on (conversation_id, sent_at DESC, id) means SQLite can seek
-- directly to the cursor position; no rows are scanned and discarded.
--
-- Params: ?1 = user_id, ?2 = channel_id,
--         ?3 = cursor_sent_at, ?4 = cursor_id, ?5 = limit
SELECT
    me.id,
    me.conversation_id,
    me.sender_id,
    u.username  AS sender_username,
    me.ciphertext,
    me.reply_to_id,
    me.sent_at
FROM message_envelope me
JOIN channels      c   ON c.id  = me.conversation_id
JOIN groups        g   ON g.id  = c.group_id
JOIN group_member  gm  ON gm.group_id = g.id AND gm.user_id = ?1
LEFT JOIN users    u   ON u.id  = me.sender_id
WHERE me.conversation_id = ?2
  AND me.type = 'message'
  AND (me.sent_at < ?3 OR (me.sent_at = ?3 AND me.id < ?4))
ORDER BY me.sent_at DESC, me.id DESC
LIMIT ?5
