-- All messages sent by a given user, ordered by group name then channel name
-- then chronologically within each channel.
-- Params: ?1 = sender_id
SELECT
    g.id   AS group_id,
    g.name AS group_name,
    c.id   AS channel_id,
    c.name AS channel_name,
    me.id,
    me.sender_id,
    me.ciphertext,
    me.sent_at
FROM message_envelope me
JOIN channels c ON c.id = me.conversation_id
JOIN groups g ON g.id = c.group_id
WHERE me.sender_id = ?1
  AND me.type = 'message'
ORDER BY g.name, c.name, me.sent_at
