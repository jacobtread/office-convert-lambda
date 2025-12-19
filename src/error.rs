use serde::Serialize;
use thiserror::Error;

#[derive(Serialize, Debug)]
pub struct LambdaErrorResponse {
    pub reason: &'static str,
    pub message: String,
}

#[derive(Debug, Error)]
pub enum LambdaError {
    #[error("failed to parse request: {0}")]
    ParseRequest(String),

    #[error("failed to make office path absolute")]
    OfficePathAbsolute,

    #[error("missing office path")]
    OfficePathMissing,

    #[error("failed to initialize office runner")]
    InitializeOffice,

    #[error("failed to create temporary directory")]
    SetupTempDirFailed,

    #[error("failed to create temporary paths")]
    SetupTempPathFailed,

    #[error("key not found in source bucket")]
    NoSuchKey,

    #[error("failed to get object from s3")]
    GetObject,

    #[error("failed to create source file")]
    CreateSourceFile,

    #[error("failed to read object chunk")]
    ReadSourceChunk,

    #[error("failed to write object chunk")]
    WriteSourceChunk,

    #[error("failed to flush object")]
    FlushSource,

    #[error("failed to create output stream")]
    CreateOutputStream,

    #[error("failed to upload output stream")]
    UploadOutputStream,

    #[error("failed to run office")]
    RunOffice,

    #[error("failed to wait for worker response")]
    ResponseError,

    #[error("failed to ready input file for integrity check")]
    ReadFileIntegrity,

    #[error("file is encrypted")]
    FileLikelyEncrypted,

    #[error("file is corrupted")]
    FileLikelyCorrupted,

    #[error("failed to convert file: {0}")]
    ConvertFailed(String),
}

impl LambdaError {
    pub fn reason(&self) -> &'static str {
        match self {
            LambdaError::ParseRequest(_) => "PARSE_REQUEST",
            LambdaError::OfficePathAbsolute => "OFFICE_PATH_ABSOLUTE",
            LambdaError::OfficePathMissing => "OFFICE_PATH_MISSING",
            LambdaError::InitializeOffice => "INITIALIZE_OFFICE",
            LambdaError::SetupTempDirFailed => "SETUP_TEMP_DIR_FAILED",
            LambdaError::SetupTempPathFailed => "SETUP_TEMP_FAILED",
            LambdaError::NoSuchKey => "NO_SUCH_KEY",
            LambdaError::GetObject => "GET_OBJECT",
            LambdaError::CreateSourceFile => "CREATE_SOURCE_FILE",
            LambdaError::ReadSourceChunk => "READ_SOURCE_CHUNK",
            LambdaError::WriteSourceChunk => "WRITE_SOURCE_CHUNK",
            LambdaError::FlushSource => "FLUSH_SOURCE",
            LambdaError::CreateOutputStream => "CREATE_OUTPUT_STREAM",
            LambdaError::UploadOutputStream => "UPLOAD_OUTPUT_STREAM",
            LambdaError::RunOffice => "RUN_OFFICE",
            LambdaError::ResponseError => "RESPONSE_ERROR",
            LambdaError::ReadFileIntegrity => "READ_FILE_INTEGRITY",
            LambdaError::FileLikelyEncrypted => "FILE_LIKELY_ENCRYPTED",
            LambdaError::FileLikelyCorrupted => "FILE_LIKELY_CORRUPTED",
            LambdaError::ConvertFailed(_) => "CONVERT_FAILED",
        }
    }
}

impl From<LambdaError> for LambdaErrorResponse {
    fn from(value: LambdaError) -> Self {
        Self {
            message: value.to_string(),
            reason: value.reason(),
        }
    }
}
