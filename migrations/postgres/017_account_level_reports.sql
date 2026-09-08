-- Allow a report that belongs to no space. See migrations/032 for why.
--
-- Postgres can drop the NOT NULL in place, so there is no table rebuild here.
ALTER TABLE reports ALTER COLUMN space_id DROP NOT NULL;

-- The instance-wide operator queue orders across every space.
CREATE INDEX IF NOT EXISTS idx_reports_created ON reports(created_at DESC);
