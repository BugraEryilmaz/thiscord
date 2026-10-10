//! Pictures are normalized server-side; clients never choose storage paths or URLs.
use super::{Failure, store};
use crate::db::DbPool;
use axum::{
    Extension,
    extract::{Path, State, rejection::PathRejection},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use diesel::{
    prelude::*,
    sql_types::{Binary, Uuid as SqlUuid},
};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, imageops::FilterType};
use std::io::Cursor;
use thiscord_shared::{AccountId, AvatarId, RequestId, account::*};
use uuid::Uuid;

fn normalize(encoded: &str) -> Result<Vec<u8>, Failure> {
    let invalid =
        || Failure::Invalid("Choose a valid PNG, JPEG or WebP up to 2 MiB and 4096 × 4096 pixels");
    if encoded.len() > MAX_AVATAR_BASE64 {
        return Err(invalid());
    }
    let bytes = STANDARD.decode(encoded).map_err(|_| invalid())?;
    if bytes.len() > MAX_AVATAR_BYTES {
        return Err(invalid());
    }
    let format = image::guess_format(&bytes).map_err(|_| invalid())?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP
    ) {
        return Err(invalid());
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|_| invalid())?;
    let orientation = decoder.orientation().map_err(|_| invalid())?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|_| invalid())?;
    if image.width() == 0 || image.height() == 0 {
        return Err(invalid());
    }
    image.apply_orientation(orientation);
    // Crop before resizing: cover-resizing a 4096 × 1 image first could allocate
    // a huge intermediate buffer even though the decoder's limits were satisfied.
    let side = image.width().min(image.height());
    let image = image
        .crop_imm(
            (image.width() - side) / 2,
            (image.height() - side) / 2,
            side,
            side,
        )
        .resize_exact(AVATAR_SIZE, AVATAR_SIZE, FilterType::Triangle)
        .to_rgba8();
    let mut output = Cursor::new(Vec::new());
    image
        .write_to(&mut output, ImageFormat::Png)
        .map_err(|_| Failure::Unavailable)?;
    Ok(output.into_inner())
}

/// Called only on the bounded account worker, with the authenticated account locked.
pub(super) fn set(
    c: &mut PgConnection,
    account: AccountId,
    encoded: Option<&str>,
) -> Result<(), Failure> {
    if let Some(encoded) = encoded {
        let png = normalize(encoded)?;
        diesel::sql_query("INSERT INTO account_avatars(account_id,id,png) VALUES($1,$2,$3) ON CONFLICT(account_id) DO UPDATE SET id=EXCLUDED.id,png=EXCLUDED.png")
            .bind::<SqlUuid, _>(account.as_uuid())
            .bind::<SqlUuid, _>(Uuid::new_v4())
            .bind::<Binary, _>(png)
            .execute(c)?;
    } else {
        store::execute(
            c,
            "DELETE FROM account_avatars WHERE account_id=$1::uuid",
            &[&account.to_string()],
        )?;
    }
    Ok(())
}

/// Public presentation media at an opaque, replaceable UUID. No account lookup/listing.
pub(super) async fn get(
    State(pool): State<Option<DbPool>>,
    Extension(request_id): Extension<RequestId>,
    path: Result<Path<AvatarId>, PathRejection>,
) -> Response {
    let result = async {
        let Path(id) = path.map_err(|_| Failure::Invalid("Invalid profile picture ID"))?;
        let pool = pool.ok_or(Failure::Unavailable)?;
        tokio::task::spawn_blocking(move || {
            #[derive(QueryableByName)]
            struct Picture {
                #[diesel(sql_type = Binary)]
                png: Vec<u8>,
            }
            let mut c = store::connection(&pool)?;
            let picture = diesel::sql_query("SELECT png FROM account_avatars WHERE id=$1")
                .bind::<SqlUuid, _>(id.as_uuid())
                .get_result::<Picture>(&mut c)
                .optional()?
                .ok_or(Failure::NotFound)?;
            Ok::<_, Failure>(picture.png)
        })
        .await
        .map_err(|_| Failure::Unavailable)?
    }
    .await;
    match result {
        Ok(png) => (
            [
                ("content-type", "image/png"),
                ("x-content-type-options", "nosniff"),
                ("cache-control", "private, max-age=300"),
            ],
            png,
        )
            .into_response(),
        Err(error) => {
            let mut response = error.response(request_id);
            response
                .headers_mut()
                .insert("cache-control", "no-store".parse().unwrap());
            response
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_supported_formats_to_a_bounded_square_png() {
        for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::WebP] {
            let mut input = Cursor::new(Vec::new());
            DynamicImage::new_rgb8(80, 40)
                .write_to(&mut input, format)
                .unwrap();
            let output = normalize(&STANDARD.encode(input.into_inner())).unwrap();
            assert_eq!(image::guess_format(&output).unwrap(), ImageFormat::Png);
            assert!(output.len() <= 300_000);
            let image = image::load_from_memory(&output).unwrap();
            assert_eq!((image.width(), image.height()), (AVATAR_SIZE, AVATAR_SIZE));
        }
    }

    #[test]
    fn rejects_malformed_unsupported_oversized_and_excessive_dimensions() {
        for input in [
            "!".into(),
            STANDARD.encode(b"<svg></svg>"),
            STANDARD.encode(b"GIF89a"),
            "a".repeat(MAX_AVATAR_BASE64 + 1),
            STANDARD.encode(vec![0; MAX_AVATAR_BYTES + 1]),
        ] {
            assert!(matches!(normalize(&input), Err(Failure::Invalid(_))));
        }
        let mut input = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(4097, 1)
            .write_to(&mut input, ImageFormat::Png)
            .unwrap();
        assert!(matches!(
            normalize(&STANDARD.encode(input.into_inner())),
            Err(Failure::Invalid(_))
        ));
    }

    #[test]
    fn extreme_aspect_ratios_are_cropped_before_upscaling() {
        for (width, height) in [(4096, 1), (1, 4096)] {
            let mut input = Cursor::new(Vec::new());
            DynamicImage::new_rgb8(width, height)
                .write_to(&mut input, ImageFormat::Png)
                .unwrap();
            let output = normalize(&STANDARD.encode(input.into_inner())).unwrap();
            let image = image::load_from_memory(&output).unwrap();
            assert_eq!((image.width(), image.height()), (AVATAR_SIZE, AVATAR_SIZE));
        }
    }
}
