use crate::{
    encrypted::{FileCondition, get_file_condition},
    error::LambdaError,
};
use aws_sdk_s3::primitives::ByteStream;
use std::{
    env::temp_dir,
    path::{Path, PathBuf, absolute},
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt},
};
use uuid::Uuid;

pub async fn setup_temp_path() -> Result<PathBuf, LambdaError> {
    let temp_path = temp_dir().join("onlyoffice-convert-server");

    // Ensure temporary path exists
    if !temp_path.exists() {
        tokio::fs::create_dir_all(&temp_path)
            .await
            .map_err(|error| {
                tracing::error!(?error, "failed to create temporary directory");
                LambdaError::SetupTempDirFailed
            })?;
    }

    Ok(temp_path)
}

pub async fn create_convert_temp_paths(temp_dir: &Path) -> std::io::Result<(PathBuf, PathBuf)> {
    // Generate random unique ID
    let random_id = Uuid::new_v4().simple();

    // Create paths in temp directory
    let input_path = temp_dir.join(format!("tmp_native_input_{random_id}"));
    let output_path = temp_dir.join(format!("tmp_native_output_{random_id}.pdf"));
    let temp_path = temp_dir.join(format!("tmp_native_temp_{random_id}"));

    // Make paths absolute
    let input_path = absolute(input_path).inspect_err(|error| {
        tracing::error!(?error, "failed to make file path absolute (input)")
    })?;
    let output_path = absolute(output_path).inspect_err(|error| {
        tracing::error!(?error, "failed to make file path absolute (output)")
    })?;
    let temp_path = absolute(temp_path)
        .inspect_err(|error| tracing::error!(?error, "failed to make file path absolute (temp)"))?;

    // Ensure temporary path exists
    if !temp_path.exists() {
        tokio::fs::create_dir_all(&temp_path).await?;
    }

    Ok((input_path, output_path))
}

/// Handles reading the first 32kb of the input file to determine the
/// condition of the file to decide the error  response message
pub async fn determine_input_file_condition(input_path: &Path) -> std::io::Result<FileCondition> {
    tracing::debug!("reading file integrity");

    let mut file = File::open(input_path).await?;
    let mut file_bytes = [0u8; 1024 * 32];
    let mut file_size: usize = 0;

    loop {
        let count = file.read(&mut file_bytes[file_size..]).await?;
        if count == 0 {
            break;
        }

        file_size += count;
    }

    file_size = file_size.min(file_bytes.len());

    tracing::debug!("finished reading file integrity, checking integrity");

    let file_condition = get_file_condition(&file_bytes[0..file_size]);
    Ok(file_condition)
}

/// Stream a file from S3 to disk
pub async fn stream_source_file(
    s3_client: &aws_sdk_s3::Client,
    source_bucket: String,
    source_key: String,
    file_path: &Path,
) -> Result<(), LambdaError> {
    let response = match s3_client
        .get_object()
        .bucket(source_bucket)
        .key(source_key)
        .send()
        .await
    {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(?error, "error streaming source file");

            if error
                .as_service_error()
                .is_some_and(|value| value.is_no_such_key())
            {
                return Err(LambdaError::NoSuchKey);
            }

            return Err(LambdaError::GetObject);
        }
    };

    let mut body = response.body;

    let mut file = tokio::fs::File::create(file_path).await.map_err(|error| {
        tracing::error!(?error, "failed to create source file");
        LambdaError::CreateSourceFile
    })?;

    while let Some(chunk_result) = body.next().await {
        let chunk = chunk_result.map_err(|error| {
            tracing::error!(?error, "failed to read object chunk");
            LambdaError::ReadSourceChunk
        })?;

        file.write_all(&chunk).await.map_err(|error| {
            tracing::error!(?error, "failed to write object chunk");
            LambdaError::WriteSourceChunk
        })?;
    }

    file.flush().await.map_err(|error| {
        tracing::error!(?error, "failed to flush object");
        LambdaError::FlushSource
    })?;

    Ok(())
}

/// Stream a file upload from disk to S3
pub async fn stream_output_file(
    s3_client: &aws_sdk_s3::Client,
    dest_bucket: String,
    dest_key: String,
    file_path: &Path,
) -> Result<(), LambdaError> {
    let byte_stream = ByteStream::from_path(file_path).await.map_err(|error| {
        tracing::error!(?error, "failed to create output stream");
        LambdaError::CreateOutputStream
    })?;

    s3_client
        .put_object()
        .bucket(dest_bucket)
        .key(dest_key)
        .body(byte_stream)
        .send()
        .await
        .map_err(|error| {
            tracing::error!(?error, "failed to upload output");
            LambdaError::UploadOutputStream
        })?;

    Ok(())
}
