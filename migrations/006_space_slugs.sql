-- Add URL-safe slug column to spaces
ALTER TABLE spaces ADD COLUMN slug TEXT NOT NULL DEFAULT '';

-- Backfill existing rows: lowercase name with spaces→hyphens + last 4 chars of ID for uniqueness
UPDATE spaces SET slug = LOWER(REPLACE(name, ' ', '-')) || '-' || SUBSTR(id, -4);

-- Ensure slugs are unique
CREATE UNIQUE INDEX idx_spaces_slug ON spaces(slug);
