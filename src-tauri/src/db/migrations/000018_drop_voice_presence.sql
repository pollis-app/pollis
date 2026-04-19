-- Drop the voice_presence table. LiveKit's RoomService is now the source
-- of truth for "who is in a voice room right now" — the shadow table we
-- maintained here was only ever a cache that drifted whenever the two
-- disagreed (crashes, force-kills, bad network). Querying LiveKit directly
-- removes the class of bug entirely.

DROP TABLE IF EXISTS voice_presence;

INSERT INTO schema_migrations (version, description)
VALUES (18, 'Drop voice_presence — LiveKit RoomService is source of truth');
