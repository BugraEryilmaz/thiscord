use axum::body::Bytes;
use futures_util::Stream;
use tokio::fs::create_dir_all;
use tokio_util::io::StreamReader;

use super::Error;



pub async fn upload_image<St>(
    mut image: StreamReader<St, Bytes>,
    filename: String,
) -> Result<String, Error> 
where St: Stream<Item = Result<Bytes, std::io::Error>> + Unpin,
{
    // extract the flieextension from the filename
    let file_extension = filename
        .rsplit('.')
        .next()
        .ok_or_else(|| Error::InvalidFileName(filename.to_string()))?;
    let file_name = format!("../images/server/{}.{}", uuid::Uuid::new_v4(), file_extension);
    let file_name_path = std::path::Path::new(&file_name);

    // Create the directory if it doesn't exist
    create_dir_all(file_name_path.parent().unwrap()).await?;
    // Open the file for writing
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(file_name_path)
        .await?;

    // Write the image data to the file
    tokio::io::copy(&mut image, &mut file).await?;

    Ok(file_name)
}
    