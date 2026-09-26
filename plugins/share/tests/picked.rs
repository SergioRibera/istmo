//! `ShareFile::from_picked` streams SAF-style files (no real path) into
//! the share backend's staging dir instead of loading them in memory.
#![cfg(all(unix, feature = "file-picker"))]

use std::os::fd::IntoRawFd;
use std::path::PathBuf;
use std::sync::Arc;

use istmo_core::{CancelToken, NativeHandleId, Runtime};
use istmo_file_picker::{
    FilePicker, FilePickerClient, FilePickerError, FilePickerHost, PickConfig, PickedFile,
    RawFileHandle, SaveConfig,
};
use istmo_share::{
    Share, ShareCapabilities, ShareClient, ShareError, ShareFile, ShareFileSource, ShareHost,
    ShareOutcome, ShareRequest, ShareTarget,
};

/// Picker that behaves like Android SAF: no filesystem path, only fds.
#[derive(Debug, Clone)]
struct SafLikePicker {
    source: PathBuf,
}

impl FilePicker for SafLikePicker {
    async fn pick_file(&self, _config: PickConfig) -> Result<Option<PickedFile>, FilePickerError> {
        Ok(Some(PickedFile {
            display_name: "../report.pdf".to_owned(),
            mime_type: Some("application/pdf".to_owned()),
            size: None,
            handle: NativeHandleId::new(1),
        }))
    }

    async fn pick_files(&self, _config: PickConfig) -> Result<Vec<PickedFile>, FilePickerError> {
        Ok(Vec::new())
    }

    async fn save_file(&self, _config: SaveConfig) -> Result<Option<PickedFile>, FilePickerError> {
        Ok(None)
    }

    async fn open_read(&self, _file: NativeHandleId) -> Result<RawFileHandle, FilePickerError> {
        let file =
            std::fs::File::open(&self.source).map_err(|e| FilePickerError::Io(e.to_string()))?;
        Ok(RawFileHandle {
            raw: i64::from(file.into_raw_fd()),
        })
    }

    async fn open_write(&self, _file: NativeHandleId) -> Result<RawFileHandle, FilePickerError> {
        Err(FilePickerError::UnsupportedOperation(
            "read-only".to_owned(),
        ))
    }

    async fn path(&self, _file: NativeHandleId) -> Result<Option<String>, FilePickerError> {
        Ok(None)
    }
}

#[derive(Debug, Clone)]
struct StagingOnly {
    dir: PathBuf,
}

impl Share for StagingOnly {
    async fn share(
        &self,
        _cancel: CancelToken,
        _request: ShareRequest,
    ) -> Result<ShareOutcome, ShareError> {
        Ok(ShareOutcome::Unknown)
    }

    async fn capabilities(&self) -> Result<ShareCapabilities, ShareError> {
        Ok(ShareCapabilities::default())
    }

    async fn staging_dir(&self) -> Result<String, ShareError> {
        Ok(self.dir.display().to_string())
    }

    async fn set_share_targets(&self, _targets: Vec<ShareTarget>) -> Result<(), ShareError> {
        Ok(())
    }
}

#[test]
fn saf_files_are_streamed_into_the_staging_dir() {
    let root = std::env::temp_dir().join(format!("istmo-share-picked-{}", std::process::id()));
    drop(std::fs::remove_dir_all(&root));
    std::fs::create_dir_all(&root).expect("root");
    let source = root.join("source.bin");
    let payload: Vec<u8> = (0..=255).cycle().take(3 * 1024 * 1024).collect();
    std::fs::write(&source, &payload).expect("source");

    let init = Runtime::mock()
        .expects::<ShareClient>()
        .expects::<FilePickerClient>()
        .host(ShareHost::new(StagingOnly {
            dir: root.join("staging"),
        }))
        .host(FilePickerHost::new(SafLikePicker { source }))
        .finish();
    let share = ShareClient::from_runtime(&init.runtime).expect("share");
    let picker = FilePickerClient::from_runtime(&init.runtime).expect("picker");

    let chosen = Arc::new(
        pollster::block_on(picker.pick_file_owned(PickConfig::default()))
            .expect("pick")
            .expect("picked"),
    );
    let file = pollster::block_on(ShareFile::from_picked(&share, &picker, &chosen)).expect("stage");

    let ShareFileSource::Path(path) = &file.source else {
        panic!("expected a staged path, got {:?}", file.source);
    };
    let path = PathBuf::from(path);
    assert!(path.starts_with(root.join("staging")), "{}", path.display());
    assert_eq!(
        path.file_name().and_then(|n| n.to_str()),
        Some("report.pdf")
    );
    assert_eq!(std::fs::read(&path).expect("staged"), payload);
    assert_eq!(file.name.as_deref(), Some("../report.pdf"));
    assert_eq!(file.mime_type.as_deref(), Some("application/pdf"));

    drop(std::fs::remove_dir_all(&root));
}
