-- Widen the reports.category allowlist with the everyday moderation reasons
-- (spam, harassment, nsfw). PostgreSQL variant of 030_report_categories.
-- See DaccordProject/daccord#204.
--
-- The old constraint was declared inline, so its name is whatever PostgreSQL
-- generated (normally `reports_category_check`). Match on the definition
-- instead of the name so an unexpected name can't leave it in place.
DO $$
DECLARE con_name text;
BEGIN
    FOR con_name IN
        SELECT c.conname
        FROM pg_constraint c
        JOIN pg_class t ON t.oid = c.conrelid
        JOIN pg_namespace n ON n.oid = t.relnamespace
        WHERE t.relname = 'reports'
          AND n.nspname = current_schema()
          AND c.contype = 'c'
          AND pg_get_constraintdef(c.oid) LIKE '%csam%'
    LOOP
        EXECUTE format('ALTER TABLE reports DROP CONSTRAINT %I', con_name);
    END LOOP;
END $$;

ALTER TABLE reports ADD CONSTRAINT reports_category_check CHECK (category IN (
    'spam', 'harassment', 'hate', 'nsfw', 'violence',
    'self_harm', 'csam', 'terrorism', 'fraud', 'other'
));
