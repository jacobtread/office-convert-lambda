use std::path::Path;

use event_handler::function_handler;
use lambda_runtime::{Error, run, service_fn, tracing};
use tokio::{
    fs::{create_dir, create_dir_all},
    task::spawn_blocking,
};

use crate::packed_libreoffice::unpack_libreoffice;

mod aws;
mod encrypted;
mod error;
mod event_handler;
mod office;
mod packed_libreoffice;
mod storage;

#[tokio::main]
async fn main() -> Result<(), Error> {
    _ = dotenvy::dotenv();

    tracing::init_default_subscriber();

    let archive_path = Path::new("/opt/libreoffice-layer.tar.zst");
    let dest_path = Path::new("/tmp");

    create_dir_all(dest_path).await?;

    tracing::debug!("unpacking libreoffice");

    spawn_blocking(|| unpack_libreoffice(archive_path, dest_path)).await??;

    tracing::debug!("unpacked libreoffice");

    unsafe {
        std::env::set_var("LIBREOFFICE_PATH", "/tmp/opt/libreoffice25.8");
        std::env::set_var("LO_PATH", "/tmp/opt/libreoffice25.8/program");
        std::env::set_var("LIBREOFFICE_SDK_PATH", "/tmp/opt/libreoffice25.8/program");
        std::env::set_var(
            "LD_LIBRARY_PATH",
            "/tmp/opt/libreoffice25.8/program:/tmp/opt/lib64:/usr/lib64",
        );
        std::env::set_var("SAL_USE_VCLPLUGIN", "gen");
        std::env::set_var("SAL_DISABLE_USERMIGRATION", "true");
        std::env::set_var("SAL_DISABLE_LOCKING", "1");
        std::env::set_var("XDG_DATA_DIRS", "/tmp/opt/share");
        std::env::set_var("HOME", "/tmp");
        std::env::set_var("UserInstallation", "file:///tmp/lo_profile");
        std::env::set_var(
            "URE_BOOTSTRAP",
            "file:///tmp/opt/libreoffice25.8/program/fundamentalrc",
        );
        std::env::set_var("FONTCONFIG_PATH", "/tmp/opt/etc/fonts");
        std::env::set_var("FONTCONFIG_FILE", "/tmp/opt/etc/fonts/fonts.conf");
    }

    create_dir("/tmp/lo_home").await?;
    create_dir("/tmp/lo_profile").await?;

    run(service_fn(function_handler)).await
}
