use std::path::Path;

use aws_sdk_s3::primitives::ByteStream;
use tokio::io::AsyncWriteExt;

use crate::error::LambdaError;

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
        Err(err) => {
            tracing::error!(?err, "error streaming source file");

            if err
                .as_service_error()
                .is_some_and(|value| value.is_no_such_key())
            {
                return Err(LambdaError::new(
                    "key not found in source bucket",
                    "NO_SUCH_KEY",
                ));
            }

            return Err(LambdaError::new(err.to_string(), "GET_OBJECT"));
        }
    };

    let mut body = response.body;

    let mut file = tokio::fs::File::create(file_path).await.map_err(|err| {
        tracing::error!(?err, "failed to create source file");
        LambdaError::new("failed to create source file", "CREATE_FILE")
    })?;

    while let Some(chunk_result) = body.next().await {
        let chunk = chunk_result.map_err(|err| {
            tracing::error!(?err, "failed to read object chunk");
            LambdaError::new("failed to read chunk", "READ_OBJECT_CHUNK")
        })?;

        file.write_all(&chunk).await.map_err(|err| {
            tracing::error!(?err, "failed to write object chunk");
            LambdaError::new("failed to write chunk", "WRITE_OBJECT_CHUNK")
        })?;
    }

    file.flush().await.map_err(|err| {
        tracing::error!(?err, "failed to flush object");
        LambdaError::new("failed to flush object", "FLUSH_OBJECT")
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
    let byte_stream = ByteStream::from_path(file_path).await.map_err(|err| {
        tracing::error!(?err, "failed to create output stream");
        LambdaError::new("failed to create output stream", "CREATE_OUTPUT_STREAM")
    })?;

    s3_client
        .put_object()
        .bucket(dest_bucket)
        .key(dest_key)
        .body(byte_stream)
        .send()
        .await
        .map_err(|err| {
            tracing::error!(?err, "failed to upload output");
            LambdaError::new("failed to upload to output stream", "UPLOAD_OUTPUT_STREAM")
        })?;

    Ok(())
}
