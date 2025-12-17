use std::{
    ffi::CStr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use libreofficekit::{CallbackType, DocUrl, Office, OfficeError, OfficeOptionalFeatures};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

/// Message for the [OfficeHandle]
pub struct ConvertMessage {
    /// Input file path
    pub input_path: PathBuf,
    /// Output file path
    pub output_path: PathBuf,
    // Channel to notify completion
    pub tx: oneshot::Sender<Result<(), OfficeConvertError>>,
}

/// Handle to send messages to the office runner
#[derive(Clone)]
pub struct OfficeHandle(pub mpsc::Sender<ConvertMessage>);

#[derive(Debug, Error)]
pub enum OfficeRunnerError {
    /// Error within LibreOffice
    #[error(transparent)]
    OfficeError(#[from] OfficeError),

    /// Error starting the runner task
    #[error("office runner task closed unexpectedly")]
    TaskClosed,
}

#[derive(Debug, Error)]
pub enum OfficeConvertError {
    #[error(transparent)]
    OfficeError(#[from] OfficeError),
    #[error("failed to convert file")]
    ConvertFailure,
}

/// Creates a new office runner on its own thread providing
/// a handle to access it via messages
pub async fn create_office_runner(path: PathBuf) -> Result<OfficeHandle, OfficeRunnerError> {
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
    startup_rx
        .await
        .map_err(|_| OfficeRunnerError::TaskClosed)??;

    let office_handle = OfficeHandle(tx);

    Ok(office_handle)
}

/// Main event loop for an office runner
fn office_runner(
    office_path: PathBuf,
    mut rx: mpsc::Receiver<ConvertMessage>,
    startup_tx: &mut Option<oneshot::Sender<Result<(), OfficeRunnerError>>>,
) -> Result<(), OfficeRunnerError> {
    // Create office instance
    let office = Office::new(&office_path)
        .inspect_err(|err| tracing::error!(?err, "failed to create office instance"))?;

    // Allow prompting for passwords
    office
        .set_optional_features(OfficeOptionalFeatures::DOCUMENT_PASSWORD)
        .inspect_err(|err| tracing::error!(?err, "failed to set optional features"))?;

    let current_input_path: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));

    office
        .register_callback({
            let paths = current_input_path.clone();
            move |office, ty, payload| {
                tracing::debug!(?ty, "callback invoked");

                if let CallbackType::DocumentPassword = ty {
                    let paths_lock = paths.lock().expect("path lock");
                    let input_path = match paths_lock.as_ref() {
                        Some(value) => value,
                        None => {
                            tracing::error!(
                                "got document password callback when no document was set"
                            );
                            return;
                        }
                    };

                    let input_url = match DocUrl::from_path(input_path) {
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
        .inspect_err(|err| tracing::error!(?err, "failed to register office callback"))?;

    // Report successful startup
    if let Some(startup_tx) = startup_tx.take() {
        _ = startup_tx.send(Ok(()));
    }

    // Get next message
    while let Some(msg) = rx.blocking_recv() {
        let ConvertMessage {
            input_path,
            output_path,
            tx,
        } = msg;

        // Set the input path for callbacks
        {
            _ = current_input_path
                .lock()
                .expect("path lock")
                .insert(input_path.clone());
        }

        // Convert document
        let result = convert_document(&office, &input_path, &output_path);

        // Send response
        _ = tx.send(result);

        // Clear the current input
        {
            *current_input_path.lock().expect("path lock") = None;
        }
    }

    Ok(())
}

/// Converts the provided document bytes into PDF format returning
/// the converted bytes
fn convert_document(
    office: &Office,
    temp_in: &Path,
    temp_out: &Path,
) -> Result<(), OfficeConvertError> {
    tracing::debug!("converting document");

    let in_url = DocUrl::from_path(temp_in)?;
    let out_url = DocUrl::from_path(temp_out)?;

    // Load document
    let mut doc = office.document_load_with_options(&in_url, "InteractionHandler=0,Batch=1")?;

    tracing::debug!("document loaded");

    // Convert document
    let result = doc.save_as(&out_url, "pdf", None)?;

    if !result {
        return Err(OfficeConvertError::ConvertFailure);
    }

    Ok(())
}
