-- Last message and sender username for every channel a given user belongs to,
-- ordered most-recently-active first (channels with no messages sort last).
-- Params: ?1 = user_id
SELECT
    g.id                       AS group_id,
    g.name                     AS group_name,
    c.id                       AS channel_id,
    c.name                     AS channel_name,
    last_msg.ciphertext        AS last_message,
    last_msg.sent_at           AS last_sent_at,
    last_msg.sender_id         AS last_sender_id,
    u.username                 AS last_sender_username
FROM channels c
JOIN groups g ON g.id = c.group_id
JOIN group_member gm ON gm.group_id = g.id AND gm.user_id = ?1
LEFT JOIN (
    SELECT me.conversation_id, me.ciphertext, me.sender_id, me.sent_at
    FROM message_envelope me
    WHERE me.type = 'message'
      AND me.sent_at = (
        SELECT MAX(sent_at)
        FROM message_envelope
        WHERE conversation_id = me.conversation_id
          AND type = 'message'
    )
) last_msg ON last_msg.conversation_id = c.id
LEFT JOIN users u ON u.id = last_msg.sender_id
ORDER BY COALESCE(last_msg.sent_at, '1970-01-01') DESC
