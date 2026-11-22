-- Message rows keep any backfilled `image` keys; older servers and clients ignore them.
ALTER TABLE direct_attachments DROP COLUMN image_height, DROP COLUMN image_width;
ALTER TABLE attachments DROP COLUMN image_height, DROP COLUMN image_width;
