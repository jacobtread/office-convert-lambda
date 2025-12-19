use aws_config::{BehaviorVersion, SdkConfig, meta::region::RegionProviderChain};

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

/// Create an S3 client from the provided `sdk_config`
pub fn s3_client(sdk_config: &SdkConfig) -> aws_sdk_s3::Client {
    let mut config = aws_sdk_s3::Config::new(sdk_config).to_builder();

    if force_path_style_env() {
        config = config.force_path_style(true);
    }

    let config = config.build();
    aws_sdk_s3::Client::from_conf(config)
}

/// FORCE_PATH_STYLE env is used when using alternative S3 servers that
/// cannot use the domain style method like MinIO
fn force_path_style_env() -> bool {
    std::env::var("FORCE_PATH_STYLE")
        //
        .is_ok_and(|value| value == "true")
}
