//! Android asset backend.
//!
//! Wraps an `android.content.res.AssetManager` handed from Kotlin. The
//! JNI helper [`install_from_java`] hands a pointer to
//! [`AAssetManager`](ndk_sys::AAssetManager); the resulting backend
//! opens assets under `<key>` and `plugin:<id>/<key>` — matching the
//! layout `emit_app` writes into `android/app/src/main/assets/`.

#[cfg(target_os = "android")]
mod imp {
    use std::ffi::CString;
    use std::io::{self, Read, Seek, SeekFrom};
    use std::ptr::NonNull;
    use std::sync::Arc;

    use istmo_core::assets::{AssetBackend, AssetReader, AssetScope, install_backend};
    use jni::JNIEnv;
    use jni::objects::JObject;

    /// SEEK_SET (see `<unistd.h>`) — POSIX absolute offset.
    const SEEK_SET: i32 = 0;
    /// SEEK_CUR — offset relative to current position.
    const SEEK_CUR: i32 = 1;
    /// SEEK_END — offset relative to end of file.
    const SEEK_END: i32 = 2;
    /// `AASSET_MODE_STREAMING` — sequential read, no full-file
    /// mmap. Fine for both `read_to_end` and short random reads that
    /// still support seek.
    const AASSET_MODE_STREAMING: i32 = 2;

    /// [`AssetBackend`] backed by an `AAssetManager*`.
    ///
    /// Cloning is cheap — the underlying manager pointer is owned by
    /// the JVM (Kotlin's `AssetManager`) and stays valid for the
    /// process lifetime.
    pub struct AndroidAssetBackend {
        manager: SendPtr,
    }

    #[derive(Clone, Copy)]
    struct SendPtr(NonNull<ndk_sys::AAssetManager>);

    // SAFETY: `AAssetManager*` is thread-safe — the NDK explicitly
    // supports concurrent `AAssetManager_open` from multiple threads.
    unsafe impl Send for SendPtr {}
    unsafe impl Sync for SendPtr {}

    impl std::fmt::Debug for AndroidAssetBackend {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("AndroidAssetBackend")
                .finish_non_exhaustive()
        }
    }

    impl AssetBackend for AndroidAssetBackend {
        fn open(&self, scope: AssetScope<'_>, key: &str) -> io::Result<AssetReader> {
            let full = match scope {
                AssetScope::App => key.to_owned(),
                AssetScope::Plugin(id) => format!("plugin:{id}/{key}"),
            };
            let cstring = CString::new(full.clone()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "asset key contains NUL byte")
            })?;
            // SAFETY: `manager` came from `AAssetManager_fromJava` and
            // is guaranteed valid for the process lifetime; `cstring`
            // is a nul-terminated pointer we own for the call.
            let asset_ptr = unsafe {
                ndk_sys::AAssetManager_open(
                    self.manager.0.as_ptr(),
                    cstring.as_ptr(),
                    AASSET_MODE_STREAMING,
                )
            };
            let asset = NonNull::new(asset_ptr).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("android asset not found: {full}"),
                )
            })?;
            Ok(AssetReader::new(AAssetReader { asset }))
        }
    }

    /// Owning wrapper around an `AAsset*` — closes on drop, forwards
    /// `Read` / `Seek` through the NDK.
    struct AAssetReader {
        asset: NonNull<ndk_sys::AAsset>,
    }

    // SAFETY: `AAsset*` is single-owner but has no thread affinity.
    // The wrapper keeps exclusive access via `&mut self` on every I/O
    // call, so cross-thread hand-off is safe as long as only one
    // thread reads at a time.
    unsafe impl Send for AAssetReader {}

    impl Read for AAssetReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            // SAFETY: `self.asset` is a valid `AAsset*` owned by this
            // wrapper; `buf` is a valid mutable slice.
            let n = unsafe {
                ndk_sys::AAsset_read(
                    self.asset.as_ptr(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                )
            };
            if n < 0 {
                Err(io::Error::other("AAsset_read returned negative"))
            } else {
                Ok(n as usize)
            }
        }
    }

    impl Seek for AAssetReader {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            let (offset, whence) = match pos {
                SeekFrom::Start(off) => (off as i64, SEEK_SET),
                SeekFrom::End(off) => (off, SEEK_END),
                SeekFrom::Current(off) => (off, SEEK_CUR),
            };
            // SAFETY: `self.asset` is a valid `AAsset*`.
            let result = unsafe { ndk_sys::AAsset_seek64(self.asset.as_ptr(), offset, whence) };
            if result < 0 {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "AAsset_seek64 failed",
                ))
            } else {
                Ok(result as u64)
            }
        }
    }

    impl Drop for AAssetReader {
        fn drop(&mut self) {
            // SAFETY: `self.asset` is a valid `AAsset*` obtained from
            // `AAssetManager_open`.
            unsafe { ndk_sys::AAsset_close(self.asset.as_ptr()) };
        }
    }

    /// Install the backend from a Java-side `AssetManager` reference.
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] with [`io::ErrorKind::InvalidInput`] when
    /// `AAssetManager_fromJava` returns null.
    pub fn install_from_java(env: &mut JNIEnv, assets: &JObject) -> io::Result<()> {
        // SAFETY: `env.get_raw()` yields a valid `JNIEnv*` for the
        // current thread; `assets.as_raw()` yields the caller's local
        // reference which is valid for the call duration.
        let raw = unsafe {
            ndk_sys::AAssetManager_fromJava(env.get_raw().cast(), assets.as_raw().cast())
        };
        let ptr = NonNull::new(raw).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "AAssetManager_fromJava returned null",
            )
        })?;
        install_backend(Arc::new(AndroidAssetBackend {
            manager: SendPtr(ptr),
        }));
        Ok(())
    }
}

#[cfg(not(target_os = "android"))]
mod imp {
    //! No-op stubs for non-Android builds so the crate keeps compiling
    //! on desktop for tests / lint / doc jobs.

    use std::io;

    use jni::JNIEnv;
    use jni::objects::JObject;

    pub fn install_from_java(_env: &mut JNIEnv, _assets: &JObject) -> io::Result<()> {
        Ok(())
    }
}

pub use imp::install_from_java;
