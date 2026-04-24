use std::fs::create_dir_all;
use std::{fs::File, path::Path};
use tar::Archive;
use zstd::Decoder;

/// Unpack the libreoffice archive to the output directory
pub fn unpack_libreoffice(archive_path: &Path, dest_path: &Path) -> std::io::Result<()> {
    let file = File::open(archive_path)
        .inspect_err(|error| tracing::error!(?error, "failed to open archive file"))?;
    let decoder = Decoder::new(file)
        .inspect_err(|error| tracing::error!(?error, "failed to create archive decoder"))?;
    let mut archive = Archive::new(decoder);

    let entries = archive
        .entries()
        .inspect_err(|error| tracing::error!(?error, "failed to get archive entries"))?;

    for entry in entries {
        let mut entry =
            entry.inspect_err(|error| tracing::error!(?error, "failed to get archive entry"))?;
        let path = entry
            .path()
            .inspect_err(|error| tracing::error!(?error, "failed to get archive entry path"))?;

        let out_path = dest_path.join(&*path);
        if !out_path.starts_with(dest_path) {
            tracing::error!(?out_path, ?dest_path, "illegal path in archive");
            return Err(std::io::Error::other(format!(
                "illegal path in archive: {:?}",
                path
            )));
        }

        if let Some(parent) = out_path.parent() {
            create_dir_all(parent).inspect_err(|error| {
                tracing::error!(?error, ?parent, "failed to create output parent directory")
            })?;
        }

        entry
            .unpack(&out_path)
            .inspect_err(|error| tracing::error!(?error, ?out_path, "failed to unpack entry"))?;
    }

    Ok(())
}
