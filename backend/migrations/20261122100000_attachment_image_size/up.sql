-- Displayed image size, read from the file header at upload (EXIF orientation
-- applied), so clients reserve the preview's exact box before downloading it.
-- NULL: not measured yet (the backend backfills older rows after startup).
-- 0 × 0: measured, not a readable PNG/JPEG/GIF/WebP image.
-- Adding nullable columns does not rewrite the tables; the checks scan them once.
SET LOCAL statement_timeout = '60s';
ALTER TABLE attachments
    ADD COLUMN image_width integer,
    ADD COLUMN image_height integer,
    ADD CONSTRAINT attachments_image_size CHECK (
        (image_width IS NULL) = (image_height IS NULL)
        AND (image_width IS NULL OR (image_width >= 0 AND image_height >= 0)));
ALTER TABLE direct_attachments
    ADD COLUMN image_width integer,
    ADD COLUMN image_height integer,
    ADD CONSTRAINT direct_attachments_image_size CHECK (
        (image_width IS NULL) = (image_height IS NULL)
        AND (image_width IS NULL OR (image_width >= 0 AND image_height >= 0)));
