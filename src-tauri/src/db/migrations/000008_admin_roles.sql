-- Collapse 'owner' role into 'admin'. Pollis now has exactly two roles: 'admin' and 'member'.
-- The creator of a group is inserted as 'admin' going forward; existing 'owner' rows are promoted.
UPDATE group_member SET role = 'admin' WHERE role = 'owner';

INSERT INTO schema_migrations (version, description) VALUES
    (8, 'collapse owner role into admin');
