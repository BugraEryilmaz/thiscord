-- This file should undo anything in `up.sql`
ALTER TABLE servers DROP COLUMN IF EXISTS image_path;