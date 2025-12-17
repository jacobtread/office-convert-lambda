use std::{
    env::temp_dir,
    ffi::CStr,
    path::{Path, PathBuf, absolute},
    sync::Arc,
};

use crate::encrypted::{self, FileCondition, get_file_condition};
use anyhow::{Context, anyhow};
use aws_config::{BehaviorVersion, SdkConfig, meta::region::RegionProviderChain};
use aws_sdk_s3::primitives::ByteStream;
use lambda_runtime::LambdaEvent;
use libreofficekit::{CallbackType, DocUrl, Office, OfficeError, OfficeOptionalFeatures};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
};
use uuid::Uuid;

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

        LambdaError {
            reason: Some("PARSE_REQUEST"),
            message: "failed to parse convert request".to_string(),
        }
    })?;

    let aws_config = aws_config().await;
    let mut config = aws_sdk_s3::Config::new(&aws_config).to_builder();

    // if std::env::var("FORCE_PATH_STYLE").is_ok_and(|value| value == "true") {
    config = config.force_path_style(true);
    // }

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

            LambdaError {
                reason: Some("OFFICE_PATH_ABSOLUTE"),
                message: "failed to make office path absolute".to_string(),
            }
        })?,
        None => {
            tracing::error!("no office path provided, cannot start server");
            panic!();
        }
    };

    // Create office access and get office details
    let office_handle = create_office_runner(office_path).await.map_err(|err| {
        tracing::error!(?err, "failed to create temporary directory");

        LambdaError {
            reason: Some("INITIALIZE_OFFICE"),
            message: "failed to create office runner".to_string(),
        }
    })?;

    let temp_path = temp_dir().join("onlyoffice-convert-server");

    // Ensure temporary path exists
    if !temp_path.exists() {
        tokio::fs::create_dir_all(&temp_path).await.map_err(|err| {
            tracing::error!(?err, "failed to create temporary directory");

            LambdaError {
                reason: Some("SETUP_TEMP_DIR_FAILED"),
                message: "failed to create temporary directory".to_string(),
            }
        })?;
    }

    // Create temporary path
    let paths = create_convert_temp_paths(&temp_path).await.map_err(|err| {
        tracing::error!(?err, "failed to setup temporary paths");
        LambdaError {
            reason: Some("SETUP_TEMP_FAILED"),
            message: "failed to setup temporary file paths".to_string(),
        }
    })?;

    let result = convert(ConvertInput {
        s3_client: &s3_client,
        paths: &paths,
        request,
        handle: office_handle,
    })
    .await;

    // Spawn a cleanup task
    tokio::spawn(async move {
        if paths.input_path.exists()
            && let Err(err) = tokio::fs::remove_file(paths.input_path).await
        {
            tracing::error!(?err, "failed to delete config file");
        }

        if paths.config_path.exists()
            && let Err(err) = tokio::fs::remove_file(paths.config_path).await
        {
            tracing::error!(?err, "failed to delete config file");
        }

        if paths.output_path.exists()
            && let Err(err) = tokio::fs::remove_file(paths.output_path).await
        {
            tracing::error!(?err, "failed to delete config file");
        }

        if paths.temp_path.exists()
            && let Err(err) = tokio::fs::remove_dir_all(paths.temp_path).await
        {
            tracing::error!(?err, "failed to remove converter temporary files");
        }
    });

    result?;

    Ok(())
}

struct ConvertInput<'a> {
    s3_client: &'a aws_sdk_s3::Client,
    paths: &'a ConvertTempPaths,
    request: ConvertRequest,
    handle: OfficeHandle,
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
        .send(OfficeMsg::Convert {
            path: input.paths.clone(),
            tx,
        })
        .await
        .map_err(|err| {
            tracing::error!(?err, "failed to run office");
            LambdaError {
                reason: Some("RUN_OFFICE"),
                message: "failed to run office".to_string(),
            }
        })?;

    // Wait for the response
    let result = rx.await.map_err(|err| {
        tracing::error!(?err, "error waiting for response");
        LambdaError {
            reason: Some("RESPONSE_ERROR"),
            message: "failed to wait for worker response".to_string(),
        }
    })?;

    tracing::debug!("convert complete");

    if let Err(err) = result {
        let message = err.to_string();

        tracing::debug!("reading file integrity");

        let mut file = tokio::fs::File::open(&input.paths.input_path)
            .await
            .map_err(|err| {
                tracing::error!(?err, "failed to open input file for integrity check");

                LambdaError {
                    reason: Some("OPEN_FILE_INTEGRITY"),
                    message: "failed to open input file for integrity check".to_string(),
                }
            })?;
        let mut file_bytes = [0u8; 1024 * 32];
        let mut file_size: usize = 0;

        loop {
            // Read a chunk into the buffer
            let n = file
                .read(&mut file_bytes[file_size..])
                .await
                .map_err(|err| {
                    tracing::error!(?err, "failed to read input file for integrity check");

                    LambdaError {
                        reason: Some("READ_FILE_INTEGRITY"),
                        message: "failed to read input file for integrity check".to_string(),
                    }
                })?;
            if n == 0 {
                break;
            }

            file_size += n;
        }

        file_size = file_size.min(file_bytes.len());

        tracing::debug!("finished reading file integrity, checking integrity");

        let file_condition = get_file_condition(&file_bytes[0..file_size]);

        tracing::error!("error processing file (file_condition = {file_condition:?}): {err}");

        // File was encrypted with a password
        if message.contains("Unsupported URL") {
            return Err(LambdaError {
                reason: Some("FILE_LIKELY_ENCRYPTED"),
                message: "file is encrypted".to_string(),
            });
        }

        // File is malformed or corrupted
        if message.contains("loadComponentFromURL returned an empty reference") {
            return match file_condition {
                encrypted::FileCondition::Normal => Err(LambdaError {
                    reason: Some("FILE_LIKELY_CORRUPTED"),
                    message: "file is corrupted".to_string(),
                }),
                encrypted::FileCondition::LikelyCorrupted => Err(LambdaError {
                    reason: Some("FILE_LIKELY_CORRUPTED"),
                    message: "file is corrupted".to_string(),
                }),
                encrypted::FileCondition::LikelyEncrypted => Err(LambdaError {
                    reason: Some("FILE_LIKELY_ENCRYPTED"),
                    message: "file is encrypted".to_string(),
                }),
            };
        }

        return Err(match file_condition {
            FileCondition::LikelyCorrupted => LambdaError {
                reason: Some("FILE_LIKELY_CORRUPTED"),
                message: "file is corrupted".to_string(),
            },
            FileCondition::LikelyEncrypted => LambdaError {
                reason: Some("FILE_LIKELY_ENCRYPTED"),
                message: "file is encrypted".to_string(),
            },
            _ => LambdaError {
                reason: None,
                message: message.to_string(),
            },
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
    config_path: PathBuf,
    input_path: PathBuf,
    temp_path: PathBuf,
    output_path: PathBuf,
}

/// Stream a file from S3 to disk
async fn stream_source_file(
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
                return Err(LambdaError {
                    reason: Some("NO_SUCH_KEY"),
                    message: "key not found in source bucket".to_string(),
                });
            }

            return Err(LambdaError {
                reason: Some("GET_OBJECT"),
                message: err.to_string(),
            });
        }
    };

    let mut body = response.body;

    let mut file = tokio::fs::File::create(file_path).await.map_err(|err| {
        tracing::error!(?err, "failed to create source file");
        LambdaError {
            reason: Some("GET_OBJECT"),
            message: err.to_string(),
        }
    })?;

    while let Some(chunk_result) = body.next().await {
        let chunk = chunk_result.map_err(|err| {
            tracing::error!(?err, "failed to read object chunk");
            LambdaError {
                reason: Some("READ_OBJECT_CHUNK"),
                message: "failed to read chunk".to_string(),
            }
        })?;

        file.write_all(&chunk).await.map_err(|err| {
            tracing::error!(?err, "failed to write object chunk");
            LambdaError {
                reason: Some("WRITE_OBJECT_CHUNK"),
                message: "failed to write chunk".to_string(),
            }
        })?;
    }

    file.flush().await.map_err(|err| {
        tracing::error!(?err, "failed to flush object");
        LambdaError {
            reason: Some("FLUSH_OBJECT"),
            message: "failed to flush object".to_string(),
        }
    })?;

    Ok(())
}

/// Stream a file upload from disk to S3
async fn stream_output_file(
    s3_client: &aws_sdk_s3::Client,
    dest_bucket: String,
    dest_key: String,
    file_path: &Path,
) -> Result<(), LambdaError> {
    let byte_stream = ByteStream::from_path(file_path).await.map_err(|err| {
        tracing::error!(?err, "failed to create output stream");
        LambdaError {
            reason: Some("CREATE_OUTPUT_STREAM"),
            message: "failed to create output stream".to_string(),
        }
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
            LambdaError {
                reason: Some("UPLOAD_OUTPUT_STREAM"),
                message: "failed to upload output stream".to_string(),
            }
        })?;

    Ok(())
}

async fn create_convert_temp_paths(temp_dir: &Path) -> std::io::Result<ConvertTempPaths> {
    // Generate random unique ID
    let random_id = Uuid::new_v4().simple();

    // Create paths in temp directory
    let config_path = temp_dir.join(format!("tmp_native_config_{random_id}.xml"));
    let input_path = temp_dir.join(format!("tmp_native_input_{random_id}"));
    let output_path = temp_dir.join(format!("tmp_native_output_{random_id}.pdf"));
    let temp_path = temp_dir.join(format!("tmp_native_temp_{random_id}"));

    // Make paths absolute
    let config_path = absolute(config_path)
        .inspect_err(|err| tracing::error!(?err, "failed to make file path absolute (config)"))?;
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
        config_path,
        input_path,
        output_path,
        temp_path,
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

#[derive(Serialize, Debug)]
pub struct LambdaError {
    pub reason: Option<&'static str>,
    pub message: String,
}

/// Messages the office runner can process
enum OfficeMsg {
    /// Message to convert a file
    Convert {
        /// Path to the temporary files
        path: ConvertTempPaths,
        /// The return channel for sending back the result
        tx: oneshot::Sender<anyhow::Result<()>>,
    },
}

/// Handle to send messages to the office runner
#[derive(Clone)]
struct OfficeHandle(mpsc::Sender<OfficeMsg>);

/// Creates a new office runner on its own thread providing
/// a handle to access it via messages
async fn create_office_runner(path: PathBuf) -> anyhow::Result<OfficeHandle> {
    let (tx, rx) = mpsc::channel(1);

    let (startup_tx, startup_rx) = oneshot::channel();

    std::thread::spawn(move || {
        let mut startup_tx = Some(startup_tx);

        if let Err(cause) = office_runner(path, rx, &mut startup_tx) {
            tracing::error!(%cause, "failed to start office runner");

            // Send the error to the startup channel if its still available
            if let Some(startup_tx) = startup_tx.take() {
                _ = startup_tx.send(Err(cause));
            }
        }
    });

    // Wait for a successful startup
    startup_rx.await.context("startup channel unavailable")??;
    let office_handle = OfficeHandle(tx);

    Ok(office_handle)
}

/// Main event loop for an office runner
fn office_runner(
    path: PathBuf,
    mut rx: mpsc::Receiver<OfficeMsg>,
    startup_tx: &mut Option<oneshot::Sender<anyhow::Result<()>>>,
) -> anyhow::Result<()> {
    // Create office instance
    let office = Office::new(&path).context("failed to create office instance")?;

    let tmp_dir = temp_dir();

    // Use our own special temp directory
    let tmp_dir = tmp_dir.join("office-convert-server");

    // Delete the temp directory if it already exists
    if tmp_dir.exists() {
        std::fs::remove_dir_all(&tmp_dir).context("failed to remove old temporary directory")?;
    }

    // create the directory
    std::fs::create_dir_all(&tmp_dir).context("failed to create temporary directory")?;

    // Allow prompting for passwords
    office
        .set_optional_features(OfficeOptionalFeatures::DOCUMENT_PASSWORD)
        .context("failed to set optional features")?;

    let current_paths: Arc<parking_lot::Mutex<Option<ConvertTempPaths>>> =
        Arc::new(parking_lot::Mutex::new(None));

    office
        .register_callback({
            let paths = current_paths.clone();
            move |office, ty, payload| {
                tracing::debug!(?ty, "callback invoked");

                if let CallbackType::DocumentPassword = ty {
                    let paths_lock = paths.lock();
                    let value = match paths_lock.as_ref() {
                        Some(value) => value,
                        None => {
                            tracing::error!(
                                "got document password callback when no document was set"
                            );
                            return;
                        }
                    };

                    let input_url = match DocUrl::from_path(&value.input_path) {
                        Ok(value) => value,
                        Err(cause) => {
                            tracing::error!(?cause, "failed to translate input path");
                            return;
                        }
                    };

                    drop(paths_lock);

                    // Provide now password
                    if let Err(cause) = office.set_document_password(&input_url, None) {
                        tracing::error!(?cause, "failed to set document password");
                    }
                }

                if let CallbackType::JSDialog = ty {
                    let payload = unsafe { CStr::from_ptr(payload) };
                    let value: serde_json::Value =
                        serde_json::from_slice(payload.to_bytes()).unwrap();

                    tracing::debug!(?value, "js dialog request");
                }
            }
        })
        .context("failed to register office callback")?;

    // Report successful startup
    if let Some(startup_tx) = startup_tx.take() {
        _ = startup_tx.send(Ok(()));
    }

    // Get next message
    while let Some(msg) = rx.blocking_recv() {
        let (path, output) = match msg {
            OfficeMsg::Convert { path, tx } => (path, tx),
        };

        {
            _ = current_paths.lock().insert(path.clone());
        }

        // Convert document
        let result = convert_document(&office, &path.input_path, &path.output_path);

        // Send response
        _ = output.send(result);

        {
            *current_paths.lock() = None;
        }
    }

    Ok(())
}

/// Converts the provided document bytes into PDF format returning
/// the converted bytes
fn convert_document(office: &Office, temp_in: &Path, temp_out: &Path) -> anyhow::Result<()> {
    tracing::debug!("converting document");

    let in_url = DocUrl::from_path(temp_in)?;
    let out_url = DocUrl::from_path(temp_out)?;

    // Load document
    let mut doc = match office.document_load_with_options(&in_url, "InteractionHandler=0,Batch=1") {
        Ok(value) => value,
        Err(err) => match err {
            OfficeError::OfficeError(err) => {
                return Err(OfficeError::OfficeError(err).into());
            }
            err => return Err(err.into()),
        },
    };

    tracing::debug!("document loaded");

    // Convert document
    let result = doc.save_as(&out_url, "pdf", None)?;

    if !result {
        return Err(anyhow!("failed to convert file"));
    }

    Ok(())
}
