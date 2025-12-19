use aws_sdk_lambda::{error::SdkError, operation::invoke::InvokeError, primitives::Blob};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use thiserror::Error;
use tokio::time::sleep;

#[derive(Debug, Clone)]
pub struct OfficeConvertLambda {
    client: aws_sdk_lambda::Client,
    options: Arc<OfficeConvertLambdaOptions>,
}

impl OfficeConvertLambda {
    pub fn new(client: aws_sdk_lambda::Client, options: OfficeConvertLambdaOptions) -> Self {
        Self {
            client,
            options: Arc::new(options),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfficeConvertLambdaOptions {
    /// The name or ARN of the Lambda function, version, or alias.
    pub function_name: String,
    /// Specify a version or alias to invoke a published version of the function.
    pub qualifier: Option<String>,
    /// The identifier of the tenant in a multi-tenant Lambda function.
    pub tenant_id: Option<String>,
    /// Number of retry attempts to perform
    pub retry_attempts: usize,
    /// Time to wait between retry attempts
    pub retry_wait: Duration,
}

impl Default for OfficeConvertLambdaOptions {
    fn default() -> Self {
        Self {
            function_name: Default::default(),
            qualifier: None,
            tenant_id: None,
            retry_attempts: 3,
            retry_wait: Duration::from_secs(1),
        }
    }
}

#[derive(Serialize)]
pub struct ConvertRequest {
    /// Bucket the input source file is within
    pub source_bucket: String,
    /// Key within the source bucket for the source file
    pub source_key: String,
    /// Bucket to store the output file
    pub dest_bucket: String,
    /// Key within the `dest_bucket` for the output file
    pub dest_key: String,
}

/// Error that could occur when requesting a conversion
#[derive(Debug, Error)]
#[allow(clippy::large_enum_variant)]
pub enum ConvertError {
    /// Failed to serialize the request or deserialize the response
    #[error(transparent)]
    Serde(#[from] serde_json::Error),

    /// Failed to convert the file
    #[error(transparent)]
    Invoke(#[from] SdkError<InvokeError>),

    /// Error from the lambda itself
    #[error(transparent)]
    Lambda(#[from] OfficeLambdaError),
}

impl ConvertError {
    pub fn is_retry(&self) -> bool {
        match self {
            // Cannot retry serde failures
            ConvertError::Serde(_) => false,
            // Should retry lambda invoke errors
            ConvertError::Invoke(_) => true,
            // Error reasons that can be retried
            ConvertError::Lambda(error) => matches!(
                error.reason.as_str(),
                "SETUP_TEMP_DIR_FAILED" | "SETUP_TEMP_FAILED" | "RUN_OFFICE" | "RESPONSE_ERROR"
            ),
        }
    }
}

impl OfficeConvertLambda {
    /// Perform an invoke of the convert lambda using a [ConvertRequest] based
    /// on files already present in S3
    ///
    /// Will attempt to retry on failure based on the current options
    pub async fn convert(&self, request: ConvertRequest) -> Result<(), ConvertError> {
        // Perform initial request
        let mut err: ConvertError = match self.convert_inner(&request).await {
            Ok(_) => return Ok(()),
            Err(error) => error,
        };

        // Perform retry attempts
        for _ in 0..self.options.retry_attempts {
            match self.convert_inner(&request).await {
                Ok(_) => return Ok(()),
                Err(error) => {
                    if !error.is_retry() {
                        return Err(error);
                    }

                    err = error;
                }
            }

            sleep(self.options.retry_wait).await;
        }

        Err(err)
    }

    /// Perform an invoke of the convert lambda using a [ConvertRequest] based
    /// on files already present in S3
    async fn convert_inner(&self, request: &ConvertRequest) -> Result<(), ConvertError> {
        let request_bytes = serde_json::to_vec(request)?;

        let output = self
            .client
            .invoke()
            .payload(Blob::new(request_bytes))
            .function_name(&self.options.function_name)
            .set_qualifier(self.options.qualifier.clone())
            .set_tenant_id(self.options.tenant_id.clone())
            .send()
            .await?;

        if let Some(function_error) = output.function_error {
            let lambda_error: OfficeLambdaError = serde_json::from_str(&function_error)?;
            return Err(ConvertError::Lambda(lambda_error));
        }

        Ok(())
    }
}

/// Error returned by the lambda
#[derive(Error, Debug, Deserialize)]
#[error("{message} ({reason})")]
pub struct OfficeLambdaError {
    pub reason: String,
    pub message: String,
}
