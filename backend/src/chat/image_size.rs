//! Displayed pixel size of image attachments, read from file headers only.
//!
//! Clients reserve a preview's exact box from this size before downloading the
//! image, so a virtualized chat row keeps its height when the image arrives
//! (docs/chat.md). Uploads measure new files; [`start_backfill`] measures files
//! stored before the columns existed and adds the size to their message rows.
use crate::{
    auth::{
        Failure,
        store::{connection, execute},
    },
    db::DbPool,
};
use diesel::{
    prelude::*,
    sql_types::{BigInt, Binary, Integer, Nullable, Uuid as SqlUuid},
};
use image::{ImageDecoder, ImageFormat, ImageReader, metadata::Orientation};
use std::{io::Cursor, time::Duration};
use thiscord_shared::chat::ImageSize;
use uuid::Uuid;

/// Size of a PNG, JPEG, GIF or WebP image as browsers display it, with JPEG and
/// PNG EXIF orientation applied. Parses headers only and never decodes pixels,
/// so it is cheap and bounded for any upload. `None` for other or unreadable files.
pub(crate) fn image_size(bytes: &[u8]) -> Option<ImageSize> {
    let format = match super::attachments::classify(bytes) {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        "image/webp" => ImageFormat::WebP,
        // The image crate is built without GIF; the screen size is fixed bytes.
        "image/gif" => {
            let width = u16::from_le_bytes(bytes.get(6..8)?.try_into().ok()?);
            let height = u16::from_le_bytes(bytes.get(8..10)?.try_into().ok()?);
            return sized(width.into(), height.into());
        }
        _ => return None,
    };
    let mut decoder = ImageReader::with_format(Cursor::new(bytes), format)
        .into_decoder()
        .ok()?;
    let (width, height) = decoder.dimensions();
    // WebP EXIF is read with an allocation sized by the chunk's declared length,
    // which an upload controls; skip it (rotated WebP files are rare). JPEG EXIF
    // segments are at most 64 KiB, and PNG chunks are read from the input.
    let orientation = if format == ImageFormat::WebP {
        Orientation::NoTransforms
    } else {
        decoder.orientation().unwrap_or(Orientation::NoTransforms)
    };
    let turned = matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    );
    if turned {
        sized(height, width)
    } else {
        sized(width, height)
    }
}

fn sized(width: u32, height: u32) -> Option<ImageSize> {
    // The columns are PostgreSQL integers.
    (width > 0 && height > 0 && width <= i32::MAX as u32 && height <= i32::MAX as u32)
        .then_some(ImageSize { width, height })
}

/// Column values for a measured file: `0 × 0` marks "not a readable image".
pub(crate) fn columns(size: Option<ImageSize>) -> (i32, i32) {
    size.map_or((0, 0), |s| (s.width as i32, s.height as i32))
}

/// The two attachment stores: guild channels and private conversations.
#[derive(Clone, Copy)]
struct Store {
    attachments: &'static str,
    messages: &'static str,
}
const STORES: [Store; 2] = [
    Store {
        attachments: "attachments",
        messages: "messages",
    },
    Store {
        attachments: "direct_attachments",
        messages: "direct_messages",
    },
];

/// Header bytes read first. PNG, GIF and WebP sizes (and JPEG EXIF, which
/// precedes the frame header) sit at the start; otherwise the file is read.
const HEAD_BYTES: i64 = 1024 * 1024;

#[derive(QueryableByName)]
struct Unmeasured {
    #[diesel(sql_type = SqlUuid)]
    id: Uuid,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    message_id: Option<Uuid>,
    #[diesel(sql_type = BigInt)]
    size_bytes: i64,
    #[diesel(sql_type = Binary)]
    head: Vec<u8>,
}

#[derive(QueryableByName)]
struct Data {
    #[diesel(sql_type = Binary)]
    data: Vec<u8>,
}

#[derive(QueryableByName)]
struct Claimed {
    #[diesel(sql_type = Nullable<SqlUuid>)]
    message_id: Option<Uuid>,
}

/// Records files that cannot be images as measured, without reading them.
fn mark_other_files(c: &mut PgConnection, store: Store) -> Result<(), Failure> {
    execute(
        c,
        &format!(
            "UPDATE {} SET image_width=0,image_height=0 WHERE image_width IS NULL AND content_type NOT IN ('image/png','image/jpeg','image/gif','image/webp')",
            store.attachments
        ),
        &[],
    )?;
    Ok(())
}

/// Adds an attachment's size to the metadata its message stores. A deleted
/// message's emptied list no longer contains the attachment and stays empty.
fn patch_message(
    c: &mut PgConnection,
    store: Store,
    message: Uuid,
    attachment: Uuid,
    size: ImageSize,
) -> Result<(), Failure> {
    execute(
        c,
        &format!(
            "UPDATE {} SET attachments=COALESCE((SELECT jsonb_agg(CASE WHEN e->>'id'=$2 THEN e || jsonb_build_object('image',jsonb_build_object('width',$3::integer,'height',$4::integer)) ELSE e END ORDER BY n) FROM jsonb_array_elements(attachments) WITH ORDINALITY AS a(e,n)),'[]'::jsonb) WHERE id=$1::uuid AND attachments @> jsonb_build_array(jsonb_build_object('id',$2::text))",
            store.messages
        ),
        &[
            &message.to_string(),
            &attachment.to_string(),
            &size.width.to_string(),
            &size.height.to_string(),
        ],
    )?;
    Ok(())
}

/// Measures one unmeasured attachment. `Ok(false)` once none remain.
fn measure_next(c: &mut PgConnection, store: Store) -> Result<bool, Failure> {
    c.transaction(|c| {
        // Reading up to a 25 MiB file can exceed the pool's 2 s statement cap.
        execute(c, "SET LOCAL statement_timeout = '60s'", &[])?;
        // Read without row locks: deletes lock the message before its
        // attachments, so the writes below take the same order.
        let Some(row) = diesel::sql_query(format!(
            "SELECT id,message_id,size_bytes,substring(data FROM 1 FOR {HEAD_BYTES}) AS head FROM {} WHERE image_width IS NULL LIMIT 1",
            store.attachments
        ))
        .load::<Unmeasured>(c)?
        .pop() else {
            return Ok(false);
        };
        let mut size = image_size(&row.head);
        if size.is_none() && row.size_bytes > HEAD_BYTES {
            let data = diesel::sql_query(format!(
                "SELECT data FROM {} WHERE id=$1",
                store.attachments
            ))
            .bind::<SqlUuid, _>(row.id)
            .load::<Data>(c)?;
            size = data.first().and_then(|d| image_size(&d.data));
        }
        if let (Some(message), Some(size)) = (row.message_id, size) {
            patch_message(c, store, message, row.id, size)?;
        }
        let (width, height) = columns(size);
        let claimed = diesel::sql_query(format!(
            "UPDATE {} SET image_width=$2,image_height=$3 WHERE id=$1 AND image_width IS NULL RETURNING message_id",
            store.attachments
        ))
        .bind::<SqlUuid, _>(row.id)
        .bind::<Integer, _>(width)
        .bind::<Integer, _>(height)
        .load::<Claimed>(c)?;
        // A staged upload sent meanwhile stored its metadata without the size.
        if row.message_id.is_none()
            && let (Some(message), Some(size)) =
                (claimed.first().and_then(|c| c.message_id), size)
        {
            patch_message(c, store, message, row.id, size)?;
        }
        Ok(true)
    })
}

/// Measures attachments stored before sizes were recorded, one file at a time
/// on a blocking worker, then stops. Transient errors (a lock held by a send or
/// delete) retry; restarts resume where it left off.
pub fn start_backfill(pool: DbPool) {
    tokio::spawn(async move {
        let mut measured = 0u64;
        for store in STORES {
            let mut failures = 0;
            let mut marked = false;
            loop {
                let pool = pool.clone();
                let step = tokio::task::spawn_blocking(move || {
                    let c = &mut *connection(&pool)?;
                    if !marked {
                        mark_other_files(c, store)?;
                    }
                    measure_next(c, store)
                })
                .await;
                match step {
                    Ok(Ok(more)) => {
                        marked = true;
                        failures = 0;
                        if !more {
                            break;
                        }
                        measured += 1;
                    }
                    _ if failures < 10 => {
                        failures += 1;
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                    _ => {
                        tracing::warn!(table = store.attachments, "Image size backfill paused");
                        return;
                    }
                }
                // Leave the pool and disk to live traffic.
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        if measured > 0 {
            tracing::info!(measured, "Image size backfill finished");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image::RgbImage::new(width, height)
            .write_to(&mut out, ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    /// A JPEG whose EXIF orientation tag says "rotate 90°" (6).
    fn rotated_jpeg(width: u32, height: u32) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image::RgbImage::new(width, height)
            .write_to(&mut out, ImageFormat::Jpeg)
            .unwrap();
        let jpeg = out.into_inner();
        // APP1 "Exif\0\0" + big-endian TIFF with one IFD entry: 0x0112 = 6.
        let mut tiff = b"MM\0\x2a\0\0\0\x08\0\x01".to_vec();
        tiff.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0]);
        tiff.extend_from_slice(&[0, 0, 0, 0]);
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let mut result = jpeg[..2].to_vec();
        result.extend_from_slice(&[0xff, 0xe1]);
        result.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        result.extend_from_slice(&app1);
        result.extend_from_slice(&jpeg[2..]);
        result
    }

    #[test]
    fn headers_give_displayed_sizes() {
        assert_eq!(
            image_size(&png(1600, 900)),
            Some(ImageSize {
                width: 1600,
                height: 900
            })
        );
        // Browsers apply EXIF orientation, so a rotated photo is displayed tall.
        assert_eq!(
            image_size(&rotated_jpeg(40, 30)),
            Some(ImageSize {
                width: 30,
                height: 40
            })
        );
        let gif = b"GIF89a\x40\x01\xf0\0\0\0\0;";
        assert_eq!(
            image_size(gif),
            Some(ImageSize {
                width: 320,
                height: 240
            })
        );
        // The backfill reads a prefix: chunks up to the image data are enough.
        let full = png(7, 5);
        let data = full.windows(4).position(|w| w == b"IDAT").unwrap();
        assert_eq!(
            image_size(&full[..data + 8]).map(|s| (s.width, s.height)),
            Some((7, 5))
        );
    }

    #[test]
    fn other_and_broken_files_have_no_size() {
        assert_eq!(image_size(b"plain text"), None);
        assert_eq!(image_size(b"\x89PNG\r\n\x1a\nbroken"), None);
        assert_eq!(image_size(b"GIF89a\0\0\x10\0"), None);
        assert_eq!(image_size(b"GIF89a"), None);
        assert_eq!(columns(None), (0, 0));
    }
}
