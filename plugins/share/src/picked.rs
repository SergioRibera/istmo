//! Share files picked with [`istmo_file_picker`].

use std::io::Read;
use std::sync::Arc;

use istmo_core::{IstmoError, codec};
use istmo_file_picker::{FilePickerClient, FilePickerError, OwnedPickedFile};

use crate::{ShareError, ShareFile, ShareFileSource};

impl ShareFile {
    /// Build a [`ShareFile`] from a file picked with
    /// [`istmo_file_picker`].
    ///
    /// Uses the file's real path when the picker exposes one (desktop,
    /// iOS/macOS while security scope is held) — keep `file` alive
    /// until the share completes in that case. Otherwise (Android SAF)
    /// the contents are read through the picker's file descriptor and
    /// shared as [`ShareFileSource::Bytes`].
    pub async fn from_picked(
        picker: &FilePickerClient,
        file: &Arc<OwnedPickedFile>,
    ) -> Result<Self, ShareError> {
        let real_path = picker
            .path(file.handle.id())
            .await
            .map_err(|err| picker_error(&decode_picker_error(err)))?;
        let source = if let Some(real_path) = real_path {
            ShareFileSource::Path(real_path)
        } else {
            let mut reader = file
                .open_reader(picker)
                .await
                .map_err(|err| picker_error(&err))?;
            let mut bytes = Vec::new();
            reader
                .read_to_end(&mut bytes)
                .map_err(|err| ShareError::Io(err.to_string()))?;
            ShareFileSource::Bytes(bytes)
        };
        Ok(Self {
            source,
            name: Some(file.display_name.clone()),
            mime_type: file.mime_type.clone(),
        })
    }
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
