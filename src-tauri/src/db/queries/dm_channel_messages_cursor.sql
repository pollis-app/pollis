-- Cursor-based pagination for DM channel messages.
-- Returns messages older than (sent_at, id) for a DM channel member.
--
-- Params: ?1 = user_id, ?2 = dm_channel_id, ?3 = cursor_sent_at, ?4 = cursor_id, ?5 = limit
SELECT me.id, me.conversation_id, me.sender_id, u.username AS sender_username, me.ciphertext, me.reply_to_id, me.sent_at
FROM message_envelope me
JOIN dm_channel_member dcm ON dcm.dm_channel_id = me.conversation_id AND dcm.user_id = ?1
LEFT JOIN users u ON u.id = me.sender_id
WHERE me.conversation_id = ?2
  AND me.type = 'message'
  AND (me.sent_at < ?3 OR (me.sent_at = ?3 AND me.id < ?4))
ORDER BY me.sent_at DESC, me.id DESC
LIMIT ?5
