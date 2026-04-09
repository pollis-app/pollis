-- Most recent N messages in a DM channel.
-- Verifies the requesting user is a member of the DM channel inline,
-- so a non-member gets zero rows rather than an auth error path.
--
-- Returns rows newest-first; the caller reverses for display if needed.
--
-- Params: ?1 = user_id, ?2 = dm_channel_id, ?3 = limit
SELECT me.id, me.conversation_id, me.sender_id, u.username AS sender_username, me.ciphertext, me.reply_to_id, me.sent_at
FROM message_envelope me
JOIN dm_channel_member dcm ON dcm.dm_channel_id = me.conversation_id AND dcm.user_id = ?1
LEFT JOIN users u ON u.id = me.sender_id
WHERE me.conversation_id = ?2
  AND me.type = 'message'
ORDER BY me.sent_at DESC, me.id DESC
LIMIT ?3
