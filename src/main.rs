use lambda_runtime::{Error, run, service_fn, tracing};
mod event_handler;
use event_handler::function_handler;
mod encrypted;

#[tokio::main]
async fn main() -> Result<(), Error> {
    _ = dotenvy::dotenv();

    tracing::init_default_subscriber();

    run(service_fn(function_handler)).await
}
