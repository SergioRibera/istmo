//! Integration test: hand-rolled `FilePicker` backend backed by a real
//! tempfile, exercising the full wire round-trip + fd bridging + std
//! trait wrappers end-to-end.

#![allow(clippy::similar_names, clippy::items_after_statements)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use istmo_core::{NativeHandleId, Runtime};
use istmo_file_picker::{
    FilePicker, FilePickerClient, FilePickerError, FilePickerHost, PickConfig, PickedFile,
    PickedFileReader, PickedFileWriter, RawFileHandle, SaveConfig,
};

#[derive(Debug)]
struct TempFileBackend {
    files: Mutex<HashMap<u64, PathBuf>>,
    next_id: Mutex<u64>,
    path: PathBuf,
    seed_contents: &'static [u8],
}

impl TempFileBackend {
    fn new(path: PathBuf, seed_contents: &'static [u8]) -> Self {
        Self {
            files: Mutex::new(HashMap::new()),
            next_id: Mutex::new(1),
            path,
            seed_contents,
        }
    }

    fn alloc(&self, path: PathBuf) -> NativeHandleId {
        let id = {
            let mut n = self.next_id.lock().expect("id counter");
            let id = *n;
            *n += 1;
            id
        };
        self.files
            .lock()
            .expect("files map")
            .insert(id, path);
        NativeHandleId::new(id)
    }

    fn lookup(&self, id: NativeHandleId) -> Result<PathBuf, FilePickerError> {
        self.files
            .lock()
            .expect("files map")
            .get(&id.get())
            .cloned()
            .ok_or_else(|| FilePickerError::NotFound(format!("handle {}", id.get())))
    }
}

impl FilePicker for TempFileBackend {
    async fn pick_file(
        &self,
        _config: PickConfig,
    ) -> Result<Option<PickedFile>, FilePickerError> {
        std::fs::write(&self.path, self.seed_contents)
            .map_err(|e| FilePickerError::Io(e.to_string()))?;
        let size = std::fs::metadata(&self.path)
            .map_err(|e| FilePickerError::Io(e.to_string()))?
            .len();
        let display_name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let handle = self.alloc(self.path.clone());
        Ok(Some(PickedFile {
            display_name,
            mime_type: Some("text/plain".into()),
            size: Some(size),
            handle,
        }))
    }

    async fn pick_files(
        &self,
        config: PickConfig,
    ) -> Result<Vec<PickedFile>, FilePickerError> {
        Ok(self.pick_file(config).await?.into_iter().collect())
    }

    async fn save_file(
        &self,
        _config: SaveConfig,
    ) -> Result<Option<PickedFile>, FilePickerError> {
        std::fs::write(&self.path, b"").map_err(|e| FilePickerError::Io(e.to_string()))?;
        let display_name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let handle = self.alloc(self.path.clone());
        Ok(Some(PickedFile {
            display_name,
            mime_type: None,
            size: Some(0),
            handle,
        }))
    }

    async fn open_read(
        &self,
        file: NativeHandleId,
    ) -> Result<RawFileHandle, FilePickerError> {
        let path = self.lookup(file)?;
        let f = std::fs::File::open(&path).map_err(|e| FilePickerError::Io(e.to_string()))?;
        Ok(RawFileHandle {
            raw: raw_from_file(f),
        })
    }

    async fn open_write(
        &self,
        file: NativeHandleId,
    ) -> Result<RawFileHandle, FilePickerError> {
        let path = self.lookup(file)?;
        let f = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| FilePickerError::Io(e.to_string()))?;
        Ok(RawFileHandle {
            raw: raw_from_file(f),
        })
    }

    async fn path(
        &self,
        file: NativeHandleId,
    ) -> Result<Option<String>, FilePickerError> {
        let path = self.lookup(file)?;
        Ok(Some(path.to_string_lossy().into_owned()))
    }
}

#[cfg(unix)]
fn raw_from_file(f: std::fs::File) -> i64 {
    use std::os::fd::IntoRawFd;
    i64::from(f.into_raw_fd())
}

#[cfg(windows)]
fn raw_from_file(f: std::fs::File) -> i64 {
    use std::os::windows::io::IntoRawHandle;
    (f.into_raw_handle() as usize) as i64
}

#[cfg(not(any(unix, windows)))]
fn raw_from_file(_f: std::fs::File) -> i64 {
    panic!("target does not support fd bridging");
}

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "istmo-file-picker-test-{}-{}-{}.tmp",
        std::process::id(),
        name,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    ))
}

fn build_runtime(backend: TempFileBackend) -> (Arc<Runtime>, FilePickerClient) {
    let init = Runtime::mock()
        .expects::<FilePickerClient>()
        .host(FilePickerHost::new(backend))
        .finish();
    let client = FilePickerClient::from_runtime(&init.runtime).expect("client declared");
    (init.runtime, client)
}

#[test]
fn pick_and_read_round_trip() {
    let path = tmp("read");
    let (_rt, picker) =
        build_runtime(TempFileBackend::new(path.clone(), b"hello from mock backend"));

    let picked = pollster::block_on(picker.pick_file_owned(PickConfig::default()))
        .expect("pick_file")
        .expect("user did not cancel");
    assert_eq!(picked.mime_type.as_deref(), Some("text/plain"));
    assert_eq!(picked.size, Some(b"hello from mock backend".len() as u64));

    let picked = Arc::new(picked);
    let mut reader = pollster::block_on(picked.open_reader(&picker)).expect("reader");
    let mut buf = String::new();
    reader.read_to_string(&mut buf).expect("read");
    assert_eq!(buf, "hello from mock backend");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn save_and_write_round_trip() {
    let path = tmp("write");
    let (_rt, picker) = build_runtime(TempFileBackend::new(path.clone(), b""));

    let saved = pollster::block_on(picker.save_file_owned(SaveConfig::default()))
        .expect("save_file")
        .expect("user did not cancel");
    let saved = Arc::new(saved);
    let mut writer = pollster::block_on(saved.open_writer(&picker)).expect("writer");
    writer.write_all(b"payload from writer").expect("write");
    writer.flush().expect("flush");
    drop(writer);

    let on_disk = std::fs::read_to_string(&path).expect("readback");
    assert_eq!(on_disk, "payload from writer");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn path_lookup_returns_stored_path() {
    let path = tmp("path");
    let (_rt, picker) =
        build_runtime(TempFileBackend::new(path.clone(), b"content"));

    let picked = pollster::block_on(picker.pick_file_owned(PickConfig::default()))
        .expect("pick_file")
        .expect("user did not cancel");
    let via_client = pollster::block_on(picker.path(picked.handle.id()))
        .expect("path returns Ok")
        .expect("wire ok");
    assert_eq!(via_client, path.to_string_lossy());

    drop(picked);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn seek_from_start_after_read() {
    let path = tmp("seek");
    let (_rt, picker) =
        build_runtime(TempFileBackend::new(path.clone(), b"first-second-third"));

    let picked = Arc::new(
        pollster::block_on(picker.pick_file_owned(PickConfig::default()))
            .expect("pick_file")
            .expect("user did not cancel"),
    );
    let mut reader = pollster::block_on(picked.open_reader(&picker)).expect("reader");

    let mut prefix = [0_u8; 5];
    reader.read_exact(&mut prefix).expect("read prefix");
    assert_eq!(&prefix, b"first");

    use std::io::Seek;
    reader
        .seek(std::io::SeekFrom::Start(0))
        .expect("rewind");
    let mut whole = String::new();
    reader.read_to_string(&mut whole).expect("read all");
    assert_eq!(whole, "first-second-third");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn release_hook_fires_on_native_handle_drop() {
    use std::sync::atomic::{AtomicU64, Ordering};

    let path = tmp("release");
    let (rt, picker) = build_runtime(TempFileBackend::new(path.clone(), b"released"));

    let released = Arc::new(AtomicU64::new(0));
    let released_hook = Arc::clone(&released);
    rt.install_native_handle_release_hook(Arc::new(move |id| {
        released_hook.store(id.get(), Ordering::Relaxed);
    }));

    let picked = pollster::block_on(picker.pick_file_owned(PickConfig::default()))
        .expect("pick_file")
        .expect("user did not cancel");
    let handle_id = picked.handle.id().get();
    assert_eq!(released.load(Ordering::Relaxed), 0);

    drop(picked);
    assert_eq!(released.load(Ordering::Relaxed), handle_id);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn writer_type_is_debug_and_readers_expose_asfd() {
    // Compile-time smoke tests over the trait objects the docs advertise.
    fn assert_read_seek<R: Read + std::io::Seek>() {}
    fn assert_write_seek<W: Write + std::io::Seek>() {}

    assert_read_seek::<PickedFileReader>();
    assert_write_seek::<PickedFileWriter>();

    #[cfg(unix)]
    fn assert_asfd<T: std::os::fd::AsFd>() {}
    #[cfg(unix)]
    {
        assert_asfd::<PickedFileReader>();
        assert_asfd::<PickedFileWriter>();
    }

    #[cfg(windows)]
    fn assert_as_handle<T: std::os::windows::io::AsHandle>() {}
    #[cfg(windows)]
    {
        assert_as_handle::<PickedFileReader>();
        assert_as_handle::<PickedFileWriter>();
    }
}
