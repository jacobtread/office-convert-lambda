use crate::{
    aws::{aws_config, s3_client},
    encrypted::{self, FileCondition},
    error::{LambdaError, LambdaErrorResponse},
    office::{OfficeHandle, create_office_runner},
    storage::{
        create_convert_temp_paths, determine_input_file_condition, setup_temp_path,
        stream_output_file, stream_source_file,
    },
};
use lambda_runtime::LambdaEvent;
use libreofficekit::Office;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf, absolute},
    str::FromStr,
    time::Duration,
};
use tokio::{sync::OnceCell, time::sleep};

/// Global dependencies store
static DEPENDENCIES: OnceCell<Dependencies> = OnceCell::const_new();

/// Dependencies used across runs
pub struct Dependencies {
    /// S3 client for uploading and downloading files
    s3_client: aws_sdk_s3::Client,
    /// Handle to LibreOffice to perform operations
    office_handle: OfficeHandle,
    /// Timeout when handling requests before the lambda is terminated forcefully.
    ///
    /// This is required in the event that the office instance because hard locked
    /// preventing requests from being handled. If this timeout is reached while
    /// attempting to convert the lambda is killed directly so that a new cold started
    /// instance can run freshly
    timeout: Duration,
}

// Try loading office path from environment variables
fn office_path_env() -> Option<PathBuf> {
    std::env::var("LIBREOFFICE_SDK_PATH")
        .map(PathBuf::from)
        .ok()
}

async fn dependencies() -> Result<Dependencies, LambdaErrorResponse> {
    let sdk_config = aws_config().await;
    let s3_client = s3_client(&sdk_config);

    let office_path: PathBuf = office_path_env()
        // Fall back to automatic detection when not provided
        .or_else(Office::find_install_path)
        // Handle no path found and none provided
        .ok_or_else(|| {
            tracing::error!("no office path provided, cannot start server");
            LambdaError::OfficePathMissing
        })?;

    // Make the path absolute
    let office_path = absolute(office_path).map_err(|err| {
        tracing::error!(?err, "failed to make x2t path absolute");
        LambdaError::OfficePathAbsolute
    })?;

    tracing::debug!(?office_path, "initializing office");

    // Create office access and get office details
    let office_handle = create_office_runner(office_path).await.map_err(|err| {
        tracing::error!(?err, "failed to create temporary directory");
        LambdaError::InitializeOffice
    })?;

    tracing::debug!("office initialized");

    // Duration in seconds for timeout
    let timeout = std::env::var("CONVERT_TIMEOUT_SECONDS")
        .ok()
        .map(|value| u64::from_str(&value))
        .transpose()
        .map_err(|_| LambdaError::InvalidConvertTimeoutSeconds)?
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(60));

    Ok(Dependencies {
        s3_client,
        office_handle,
        timeout,
    })
}

#[derive(Serialize)]
pub struct Output {
    success: bool,
}

pub(crate) async fn function_handler(
    event: LambdaEvent<Value>,
) -> Result<Output, lambda_runtime::Error> {
    if let Err(error) = handle_request(event).await {
        let error_json = serde_json::to_string(&error)?;
        return Err(lambda_runtime::Error::from(error_json));
    }

    Ok(Output { success: true })
}

type BucketName = String;

type BucketKey = String;

#[derive(Deserialize)]
struct ConvertRequest {
    /// Bucket the input source file is within
    source_bucket: BucketName,
    /// Key within the source bucket for the source file
    source_key: BucketKey,
    /// Bucket to store the output file
    dest_bucket: BucketName,
    /// Key within the `dest_bucket` for the output file
    dest_key: BucketKey,
}

async fn handle_request(event: LambdaEvent<Value>) -> Result<(), LambdaErrorResponse> {
    let request: ConvertRequest = serde_json::from_value(event.payload).map_err(|error| {
        tracing::error!(?error, "failed to parse request");
        LambdaError::ParseRequest(error.to_string())
    })?;

    let dependencies = DEPENDENCIES.get_or_try_init(dependencies).await?;
    tracing::debug!("initialized dependencies");

    let temp_path = setup_temp_path().await?;

    let (input_path, output_path) =
        create_convert_temp_paths(&temp_path)
            .await
            .map_err(|error| {
                tracing::error!(?error, "failed to setup temporary paths");
                LambdaError::SetupTempPathFailed
            })?;

    let convert_future = convert(
        &dependencies.s3_client,
        &dependencies.office_handle,
        request,
        &input_path,
        &output_path,
    );

    let timeout_future = sleep(dependencies.timeout);

    tokio::select! {
        result = convert_future => {
            result?;

            // Spawn background cleanup
            tokio::spawn(cleanup(input_path, output_path));
        },
        _ = timeout_future => {
            abort();
        }
    }

    Ok(())
}

/// Internal abortion logic, report the failure to log and
/// abort the program. This state is only reached if the timeout
/// for converting has been reached and is used to force the lambda
/// to use a new process instance
fn abort() {
    tracing::error!("aborted due to timeout exceeded");
    std::process::abort();
}

async fn convert(
    s3_client: &aws_sdk_s3::Client,
    handle: &OfficeHandle,
    request: ConvertRequest,
    input_path: &Path,
    output_path: &Path,
) -> Result<(), LambdaError> {
    tracing::debug!("streaming source file");

    // Stream the input file to disk
    stream_source_file(
        s3_client,
        request.source_bucket,
        request.source_key,
        input_path,
    )
    .await?;

    tracing::debug!("running converter");

    if let Err(error) = handle
        .convert(input_path.to_path_buf(), output_path.to_path_buf())
        .await?
    {
        let message = error.to_string();

        let file_condition = determine_input_file_condition(input_path)
            .await
            .map_err(|error| {
                tracing::error!(?error, "failed to open input file for integrity check");
                LambdaError::ReadFileIntegrity
            })?;

        tracing::error!(?error, ?file_condition, ?message, "error processing file");

        // File was encrypted with a password
        if message.contains("Unsupported URL") {
            return Err(LambdaError::FileLikelyEncrypted);
        }

        // File is malformed or corrupted
        if message.contains("loadComponentFromURL returned an empty reference") {
            return Err(match file_condition {
                encrypted::FileCondition::Normal => LambdaError::FileLikelyCorrupted,
                encrypted::FileCondition::LikelyCorrupted => LambdaError::FileLikelyCorrupted,
                encrypted::FileCondition::LikelyEncrypted => LambdaError::FileLikelyEncrypted,
            });
        }

        return Err(match file_condition {
            FileCondition::LikelyCorrupted => LambdaError::FileLikelyCorrupted,
            FileCondition::LikelyEncrypted => LambdaError::FileLikelyEncrypted,
            _ => LambdaError::ConvertFailed(message),
        });
    }

    tracing::debug!("convert complete");

    stream_output_file(
        s3_client,
        request.dest_bucket,
        request.dest_key,
        output_path,
    )
    .await?;

    Ok(())
}

async fn cleanup(input_path: PathBuf, output_path: PathBuf) {
    if input_path.exists()
        && let Err(error) = tokio::fs::remove_file(input_path).await
    {
        tracing::error!(?error, "failed to delete config file");
    }

    if output_path.exists()
        && let Err(error) = tokio::fs::remove_file(output_path).await
    {
        tracing::error!(?error, "failed to delete config file");
    }
}
