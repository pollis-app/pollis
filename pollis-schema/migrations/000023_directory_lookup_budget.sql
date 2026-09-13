-- #1089: a per-user daily budget for directory identifier lookups.
--
-- `/v1/directory/users` resolves an `identifier` — a username OR an email
-- address — to an account id, username and avatar, for any authenticated
-- caller at the read tier. That makes any account holder an email→identity
-- oracle: feed it a list of addresses and it tells you which have Pollis
-- accounts and under what name.
--
-- The per-IP middleware tier sheds floods but an attacker rotates IPs, so the
-- bound that actually binds has to be keyed on the authenticated user and read
-- from the database — the same reasoning `groups::apply_redeem_invite_link`
-- already writes down for invite redemption. It survives a DS restart and is
-- shared across container instances.
--
-- A per-(user, day) COUNTER rather than an audit row per lookup: one small row
-- per active user per day, no unbounded growth, and nothing to prune urgently.
-- It deliberately records no identifier and no result — the point is to bound
-- the oracle, not to build a log of who looked up whom, which would be a worse
-- privacy leak than the one being fixed.
CREATE TABLE IF NOT EXISTS directory_lookup_budget (
  user_id TEXT NOT NULL,
  -- UTC calendar day, 'YYYY-MM-DD'.
  day     TEXT NOT NULL,
  lookups INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (user_id, day)
);
