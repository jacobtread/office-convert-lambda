use serde::Serialize;

#[derive(Serialize, Debug)]
pub struct LambdaError {
    pub reason: &'static str,
    pub message: String,
}

impl LambdaError {
    pub fn new(message: impl Into<String>, reason: &'static str) -> Self {
        Self {
            message: message.into(),
            reason,
        }
    }
}
