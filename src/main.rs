use event_handler::function_handler;
use lambda_runtime::{Error, run, service_fn, tracing};

mod aws;
mod encrypted;
mod error;
mod event_handler;
mod office;
mod storage;

#[tokio::main]
async fn main() -> Result<(), Error> {
    _ = dotenvy::dotenv();

    tracing::init_default_subscriber();

    run(service_fn(function_handler)).await
}
