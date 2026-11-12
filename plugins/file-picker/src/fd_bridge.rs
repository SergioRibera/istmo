//! `std::io::{Read, Write, Seek}` wrappers over the fd/HANDLE returned
//! by [`FilePicker::open_read`](crate::FilePicker::open_read) /
//! [`FilePicker::open_write`](crate::FilePicker::open_write).
//!
//! These types adopt the raw handle into a [`std::fs::File`] and hold an
//! `Arc<OwnedPickedFile>` so the underlying
//! [`NativeHandle`](istmo_core::NativeHandle) stays live for as long as
//! the reader / writer does — dropping either the reader / writer or the
//! last outstanding `Arc` releases the native resource.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use istmo_core::IstmoError;

use crate::{FilePickerClient, FilePickerError, OwnedPickedFile, RawFileHandle};

/// Read half of a picked file. Implements [`Read`] + [`Seek`] plus the
/// platform's `AsFd` / `AsHandle` traits.
#[derive(Debug)]
pub struct PickedFileReader {
    file: std::fs::File,
    _keep: Arc<OwnedPickedFile>,
}

/// Write half of a picked file. Implements [`Write`] + [`Seek`] plus the
/// platform's `AsFd` / `AsHandle` traits.
#[derive(Debug)]
pub struct PickedFileWriter {
    file: std::fs::File,
    _keep: Arc<OwnedPickedFile>,
}

impl PickedFileReader {
    /// Open the picked file for reading.
    ///
    /// The `keep_alive` `Arc` is retained by the returned reader so the
    /// backing [`NativeHandle`](istmo_core::NativeHandle) is not
    /// released — even if the caller's own `Arc` is dropped first.
    pub async fn open(
        picker: &FilePickerClient,
        keep_alive: Arc<OwnedPickedFile>,
    ) -> Result<Self, FilePickerError> {
        let raw = call_open_read(picker, keep_alive.handle.id()).await?;
        Ok(Self {
            file: file_from_raw(raw)?,
            _keep: keep_alive,
        })
    }

    /// Consume the wrapper and return the underlying [`std::fs::File`].
    ///
    /// The native handle is released as soon as the caller drops the
    /// returned file — the `Arc<OwnedPickedFile>` reference held by the
    /// wrapper is dropped here, and the file itself closes the fd on
    /// its own drop.
    #[must_use]
    pub fn into_inner(self) -> std::fs::File {
        self.file
    }
}

impl PickedFileWriter {
    /// Open the picked file for writing. See [`PickedFileReader::open`].
    pub async fn open(
        picker: &FilePickerClient,
        keep_alive: Arc<OwnedPickedFile>,
    ) -> Result<Self, FilePickerError> {
        let raw = call_open_write(picker, keep_alive.handle.id()).await?;
        Ok(Self {
            file: file_from_raw(raw)?,
            _keep: keep_alive,
        })
    }

    /// Consume the wrapper and return the underlying [`std::fs::File`].
    #[must_use]
    pub fn into_inner(self) -> std::fs::File {
        self.file
    }
}

impl OwnedPickedFile {
    /// Convenience shortcut for [`PickedFileReader::open`] — wraps the
    /// current `Arc` and awaits the open.
    pub async fn open_reader(
        self: &Arc<Self>,
        picker: &FilePickerClient,
    ) -> Result<PickedFileReader, FilePickerError> {
        PickedFileReader::open(picker, Arc::clone(self)).await
    }

    /// Convenience shortcut for [`PickedFileWriter::open`].
    pub async fn open_writer(
        self: &Arc<Self>,
        picker: &FilePickerClient,
    ) -> Result<PickedFileWriter, FilePickerError> {
        PickedFileWriter::open(picker, Arc::clone(self)).await
    }
}

impl Read for PickedFileReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
    fn read_vectored(&mut self, bufs: &mut [io::IoSliceMut<'_>]) -> io::Result<usize> {
        self.file.read_vectored(bufs)
    }
    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        self.file.read_to_end(buf)
    }
    fn read_to_string(&mut self, buf: &mut String) -> io::Result<usize> {
        self.file.read_to_string(buf)
    }
}

impl Seek for PickedFileReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
    fn stream_position(&mut self) -> io::Result<u64> {
        self.file.stream_position()
    }
}

impl Write for PickedFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        self.file.write_vectored(bufs)
    }
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.file.write_all(buf)
    }
}

impl Seek for PickedFileWriter {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
    fn stream_position(&mut self) -> io::Result<u64> {
        self.file.stream_position()
    }
}

// ------------------------------------------------------------------ platform fd traits

#[cfg(unix)]
mod unix_fd {
    use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};

    use super::{PickedFileReader, PickedFileWriter};

    impl AsFd for PickedFileReader {
        fn as_fd(&self) -> BorrowedFd<'_> {
            self.file.as_fd()
        }
    }
    impl AsRawFd for PickedFileReader {
        fn as_raw_fd(&self) -> RawFd {
            self.file.as_raw_fd()
        }
    }
    impl AsFd for PickedFileWriter {
        fn as_fd(&self) -> BorrowedFd<'_> {
            self.file.as_fd()
        }
    }
    impl AsRawFd for PickedFileWriter {
        fn as_raw_fd(&self) -> RawFd {
            self.file.as_raw_fd()
        }
    }
}

#[cfg(windows)]
mod windows_handle {
    use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, RawHandle};

    use super::{PickedFileReader, PickedFileWriter};

    impl AsHandle for PickedFileReader {
        fn as_handle(&self) -> BorrowedHandle<'_> {
            self.file.as_handle()
        }
    }
    impl AsRawHandle for PickedFileReader {
        fn as_raw_handle(&self) -> RawHandle {
            self.file.as_raw_handle()
        }
    }
    impl AsHandle for PickedFileWriter {
        fn as_handle(&self) -> BorrowedHandle<'_> {
            self.file.as_handle()
        }
    }
    impl AsRawHandle for PickedFileWriter {
        fn as_raw_handle(&self) -> RawHandle {
            self.file.as_raw_handle()
        }
    }
}

// ------------------------------------------------------------------ fd bridge

// The only `unsafe` in the crate: adopting the fd / `HANDLE` the native
// side hands over. The crate lints set `unsafe_code = "deny"` (not the
// workspace's `forbid`, which cannot be lifted locally) so these two
// functions can opt out one by one.

#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "adopting a native-owned fd needs `File::from_raw_fd`"
)]
fn file_from_raw(raw: RawFileHandle) -> Result<std::fs::File, FilePickerError> {
    use std::os::fd::{FromRawFd, RawFd};
    // The wire carries an `i64`; a value that does not fit a `RawFd` or
    // is negative cannot be an fd the native side opened, and adopting
    // a truncated number would close some unrelated descriptor later.
    let fd = RawFd::try_from(raw.raw)
        .ok()
        .filter(|fd| *fd >= 0)
        .ok_or_else(|| FilePickerError::Io(format!("invalid file descriptor {}", raw.raw)))?;
    // SAFETY: `fd` is a non-negative descriptor produced by a successful
    // open_read/open_write on the native side, which has detached it
    // (Android `ParcelFileDescriptor.detachFd`) or returned an owned
    // `open()` result (iOS/macOS/Linux/desktop), so it is open and this
    // process's Rust side is its sole owner. `File` becomes its closer.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "adopting a native-owned HANDLE needs `File::from_raw_handle`"
)]
fn file_from_raw(raw: RawFileHandle) -> Result<std::fs::File, FilePickerError> {
    use std::os::windows::io::{FromRawHandle, RawHandle};
    // Kernel handles are positive; `0`, `INVALID_HANDLE_VALUE` (-1) and
    // the negative pseudo-handles are never files the backend opened.
    let handle = isize::try_from(raw.raw)
        .ok()
        .filter(|h| *h > 0)
        .ok_or_else(|| FilePickerError::Io(format!("invalid file handle {}", raw.raw)))?;
    // SAFETY: `handle` is a positive `HANDLE` the native side opened for
    // the requested access mode and handed over with sole ownership.
    Ok(unsafe { std::fs::File::from_raw_handle(handle as RawHandle) })
}

#[cfg(not(any(unix, windows)))]
fn file_from_raw(_raw: RawFileHandle) -> Result<std::fs::File, FilePickerError> {
    Err(FilePickerError::UnsupportedOperation(
        "fd bridging is not supported on this target".into(),
    ))
}

// ------------------------------------------------------------------ error decoding

async fn call_open_read(
    picker: &FilePickerClient,
    id: istmo_core::NativeHandleId,
) -> Result<RawFileHandle, FilePickerError> {
    map_err(picker.open_read(id).await)
}

async fn call_open_write(
    picker: &FilePickerClient,
    id: istmo_core::NativeHandleId,
) -> Result<RawFileHandle, FilePickerError> {
    map_err(picker.open_write(id).await)
}

fn map_err(res: Result<RawFileHandle, IstmoError>) -> Result<RawFileHandle, FilePickerError> {
    match res {
        Ok(raw) => Ok(raw),
        Err(IstmoError::PluginError { bytes }) => {
            let (err, _) = istmo_core::codec::decode::<FilePickerError>(&bytes)
                .map_err(|e| FilePickerError::Io(format!("decode plugin error payload: {e}")))?;
            Err(err)
        }
        Err(other) => Err(FilePickerError::Backend(other.to_string())),
    }
}
