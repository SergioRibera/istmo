//! Share files picked with [`istmo_file_picker`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use istmo_core::{IstmoError, codec};
use istmo_file_picker::{FilePickerClient, FilePickerError, OwnedPickedFile};

use crate::{ShareClient, ShareError, ShareFile, ShareFileSource};

impl ShareFile {
    /// Build a [`ShareFile`] from a file picked with
    /// [`istmo_file_picker`].
    ///
    /// Uses the file's real path when the picker exposes one (desktop,
    /// iOS/macOS while security scope is held) — keep `file` alive until
    /// the share completes in that case. Otherwise (Android SAF) the
    /// contents are streamed through the picker's file descriptor into
    /// the share backend's [staging directory](crate::Share::staging_dir),
    /// in constant memory, and shared from there.
    pub async fn from_picked(
        share: &ShareClient,
        picker: &FilePickerClient,
        file: &Arc<OwnedPickedFile>,
    ) -> Result<Self, ShareError> {
        let real_path = picker
            .path(file.handle.id())
            .await
            .map_err(|err| picker_error(&decode_picker_error(err)))?;
        let path = match real_path {
            Some(path) => path,
            None => stream_into_staging(share, picker, file).await?,
        };
        Ok(Self {
            source: ShareFileSource::Path(path),
            name: Some(file.display_name.clone()),
            mime_type: file.mime_type.clone(),
        })
    }
}

async fn stream_into_staging(
    share: &ShareClient,
    picker: &FilePickerClient,
    file: &Arc<OwnedPickedFile>,
) -> Result<String, ShareError> {
    let staging = PathBuf::from(share.staging_dir().await.map_err(ShareError::from)?);
    let dir = staging.join(unique_dir_name());
    std::fs::create_dir_all(&dir).map_err(|err| io_error(&dir, &err))?;
    let dest = dir.join(crate::sanitize_file_name(&file.display_name));

    let mut reader = file
        .open_reader(picker)
        .await
        .map_err(|err| picker_error(&err))?;
    let mut out = std::fs::File::create(&dest).map_err(|err| io_error(&dest, &err))?;
    std::io::copy(&mut reader, &mut out).map_err(|err| io_error(&dest, &err))?;
    Ok(dest.display().to_string())
}

fn unique_dir_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("picked-{}-{nanos}", std::process::id())
}

fn io_error(path: &std::path::Path, err: &std::io::Error) -> ShareError {
    ShareError::Io(format!("{}: {err}", path.display()))
}

fn decode_picker_error(err: IstmoError) -> FilePickerError {
    match err {
        IstmoError::PluginError { bytes } => codec::decode::<FilePickerError>(&bytes)
            .map_or_else(|e| FilePickerError::Backend(e.to_string()), |(err, _)| err),
        other => FilePickerError::Backend(other.to_string()),
    }
}

fn picker_error(err: &FilePickerError) -> ShareError {
    match err {
        FilePickerError::NotFound(msg) => ShareError::NotFound(msg.clone()),
        FilePickerError::Io(msg) => ShareError::Io(msg.clone()),
        other => ShareError::Backend(other.to_string()),
    }
}
