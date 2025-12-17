use std::{
    env::temp_dir,
    path::{Path, PathBuf, absolute},
};

use crate::{
    encrypted::{self, FileCondition, get_file_condition},
    error::LambdaError,
    office::{ConvertMessage, OfficeHandle, create_office_runner},
    storage::{stream_output_file, stream_source_file},
};
use aws_config::{BehaviorVersion, SdkConfig, meta::region::RegionProviderChain};
use lambda_runtime::LambdaEvent;
use libreofficekit::Office;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    io::AsyncReadExt,
    sync::{OnceCell, oneshot},
};
use uuid::Uuid;

static DEPENDENCIES: OnceCell<Dependencies> = OnceCell::const_new();

pub struct Dependencies {
    /// S3 client for uploading and downloading files
    s3_client: aws_sdk_s3::Client,
    /// Handle to LibreOffice to perform operations
    office_handle: OfficeHandle,
}

async fn dependencies() -> Result<Dependencies, LambdaError> {
    let aws_config = aws_config().await;
    let mut config = aws_sdk_s3::Config::new(&aws_config).to_builder();

    if std::env::var("FORCE_PATH_STYLE").is_ok_and(|value| value == "true") {
        config = config.force_path_style(true);
    }

    let config = config.build();
    let s3_client = aws_sdk_s3::Client::from_conf(config);

    let mut office_path: Option<PathBuf> = None;

    // Try loading office path from environment variables
    if office_path.is_none()
        && let Ok(path) = std::env::var("LIBREOFFICE_SDK_PATH")
    {
        office_path = Some(PathBuf::from(&path));
    }

    // Try determine default office path
    if office_path.is_none() {
        office_path = Office::find_install_path();
    }

    // Check a path was provided
    let office_path = match office_path {
        Some(value) => absolute(value).map_err(|err| {
            tracing::error!(?err, "failed to make x2t path absolute");
            LambdaError::new(
                "failed to make office path absolute",
                "OFFICE_PATH_ABSOLUTE",
            )
        })?,
        None => {
            tracing::error!("no office path provided, cannot start server");
            panic!();
        }
    };

    // Create office access and get office details
    let office_handle = create_office_runner(office_path).await.map_err(|err| {
        tracing::error!(?err, "failed to create temporary directory");
        LambdaError::new("failed to create office runner", "INITIALIZE_OFFICE")
    })?;

    Ok(Dependencies {
        s3_client,
        office_handle,
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

async fn handle_request(event: LambdaEvent<Value>) -> Result<(), LambdaError> {
    let request: ConvertRequest = serde_json::from_value(event.payload).map_err(|err| {
        tracing::error!(?err, "failed to parse request");
        LambdaError::new(err.to_string(), "PARSE_REQUEST")
    })?;

    let dependencies = DEPENDENCIES.get_or_try_init(dependencies).await?;

    let temp_path = temp_dir().join("onlyoffice-convert-server");

    // Ensure temporary path exists
    if !temp_path.exists() {
        tokio::fs::create_dir_all(&temp_path).await.map_err(|err| {
            tracing::error!(?err, "failed to create temporary directory");
            LambdaError::new(
                "failed to create temporary directory",
                "SETUP_TEMP_DIR_FAILED",
            )
        })?;
    }

    // Create temporary path
    let paths = create_convert_temp_paths(&temp_path).await.map_err(|err| {
        tracing::error!(?err, "failed to setup temporary paths");
        LambdaError::new("failed to setup temporary file paths", "SETUP_TEMP_FAILED")
    })?;

    let result = convert(ConvertInput {
        s3_client: &dependencies.s3_client,
        paths: &paths,
        request,
        handle: &dependencies.office_handle,
    })
    .await;

    // Spawn a cleanup task
    tokio::spawn(async move {
        if paths.input_path.exists()
            && let Err(err) = tokio::fs::remove_file(paths.input_path).await
        {
            tracing::error!(?err, "failed to delete config file");
        }

        if paths.output_path.exists()
            && let Err(err) = tokio::fs::remove_file(paths.output_path).await
        {
            tracing::error!(?err, "failed to delete config file");
        }
    });

    result?;

    Ok(())
}

struct ConvertInput<'a> {
    s3_client: &'a aws_sdk_s3::Client,
    paths: &'a ConvertTempPaths,
    request: ConvertRequest,
    handle: &'a OfficeHandle,
}

async fn convert(input: ConvertInput<'_>) -> Result<(), LambdaError> {
    tracing::debug!("streaming source file");

    // Stream the input file to disk
    stream_source_file(
        input.s3_client,
        input.request.source_bucket,
        input.request.source_key,
        &input.paths.input_path,
    )
    .await?;

    tracing::debug!("running converter");
    let (tx, rx) = oneshot::channel();

    input
        .handle
        .0
        .send(ConvertMessage {
            input_path: input.paths.input_path.clone(),
            output_path: input.paths.output_path.clone(),
            tx,
        })
        .await
        .map_err(|err| {
            tracing::error!(?err, "failed to run office");
            LambdaError::new("failed to run office", "RUN_OFFICE")
        })?;

    // Wait for the response
    let result = rx.await.map_err(|err| {
        tracing::error!(?err, "error waiting for response");
        LambdaError::new("failed to wait for worker response", "RESPONSE_ERROR")
    })?;

    tracing::debug!("convert complete");

    if let Err(err) = result {
        let message = err.to_string();

        let file_condition = determine_input_file_condition(&input.paths.input_path)
            .await
            .map_err(|err| {
                tracing::error!(?err, "failed to open input file for integrity check");
                LambdaError::new(
                    "failed to read input file for integrity check",
                    "READ_FILE_INTEGRITY",
                )
            })?;

        tracing::error!("error processing file (file_condition = {file_condition:?}): {err}");

        // File was encrypted with a password
        if message.contains("Unsupported URL") {
            return Err(LambdaError::new(
                "file is encrypted",
                "FILE_LIKELY_ENCRYPTED",
            ));
        }

        // File is malformed or corrupted
        if message.contains("loadComponentFromURL returned an empty reference") {
            return match file_condition {
                encrypted::FileCondition::Normal => Err(LambdaError::new(
                    "file is corrupted",
                    "FILE_LIKELY_CORRUPTED",
                )),
                encrypted::FileCondition::LikelyCorrupted => Err(LambdaError::new(
                    "file is corrupted",
                    "FILE_LIKELY_CORRUPTED",
                )),
                encrypted::FileCondition::LikelyEncrypted => Err(LambdaError::new(
                    "file is encrypted",
                    "FILE_LIKELY_ENCRYPTED",
                )),
            };
        }

        return Err(match file_condition {
            FileCondition::LikelyCorrupted => {
                LambdaError::new("file is corrupted", "FILE_LIKELY_CORRUPTED")
            }
            FileCondition::LikelyEncrypted => {
                LambdaError::new("file is encrypted", "FILE_LIKELY_ENCRYPTED")
            }
            _ => LambdaError::new(message, "CONVERT_FAILED"),
        });
    }

    stream_output_file(
        input.s3_client,
        input.request.dest_bucket,
        input.request.dest_key,
        &input.paths.output_path,
    )
    .await?;

    Ok(())
}

/// Handles reading the first 32kb of the input file to determine the
/// condition of the file to decide the error  response message
async fn determine_input_file_condition(input_path: &Path) -> std::io::Result<FileCondition> {
    tracing::debug!("reading file integrity");

    let mut file = tokio::fs::File::open(input_path).await?;
    let mut file_bytes = [0u8; 1024 * 32];
    let mut file_size: usize = 0;

    loop {
        let n = file.read(&mut file_bytes[file_size..]).await?;
        if n == 0 {
            break;
        }

        file_size += n;
    }

    file_size = file_size.min(file_bytes.len());

    tracing::debug!("finished reading file integrity, checking integrity");

    let file_condition = get_file_condition(&file_bytes[0..file_size]);
    Ok(file_condition)
}

#[derive(Deserialize)]
struct ConvertRequest {
    /// Bucket the input source file is within
    source_bucket: String,
    /// Key within the source bucket for the source file
    source_key: String,
    /// Bucket to store the output file
    dest_bucket: String,
    /// Key within the `dest_bucket` for the output file
    dest_key: String,
}

#[derive(Clone)]
struct ConvertTempPaths {
    input_path: PathBuf,
    output_path: PathBuf,
}

async fn create_convert_temp_paths(temp_dir: &Path) -> std::io::Result<ConvertTempPaths> {
    // Generate random unique ID
    let random_id = Uuid::new_v4().simple();

    // Create paths in temp directory
    let input_path = temp_dir.join(format!("tmp_native_input_{random_id}"));
    let output_path = temp_dir.join(format!("tmp_native_output_{random_id}.pdf"));
    let temp_path = temp_dir.join(format!("tmp_native_temp_{random_id}"));

    // Make paths absolute
    let input_path = absolute(input_path)
        .inspect_err(|err| tracing::error!(?err, "failed to make file path absolute (input)"))?;
    let output_path = absolute(output_path)
        .inspect_err(|err| tracing::error!(?err, "failed to make file path absolute (output)"))?;
    let temp_path = absolute(temp_path)
        .inspect_err(|err| tracing::error!(?err, "failed to make file path absolute (temp)"))?;

    // Ensure temporary path exists
    if !temp_path.exists() {
        tokio::fs::create_dir_all(&temp_path).await?;
    }

    Ok(ConvertTempPaths {
        input_path,
        output_path,
    })
}

/// Create the AWS production configuration
pub async fn aws_config() -> SdkConfig {
    let region_provider = RegionProviderChain::default_provider()
        // Fallback to our desired region
        .or_else("ap-southeast-2");

    // Load the configuration from env variables (See https://docs.aws.amazon.com/sdkref/latest/guide/settings-reference.html#EVarSettings)
    aws_config::defaults(BehaviorVersion::v2025_08_07())
        // Setup the region provider
        .region(region_provider)
        .load()
        .await
}
