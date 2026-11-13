//! Android asset backend.
//!
//! Wraps an `android.content.res.AssetManager` handed from Kotlin. The
//! JNI helper [`install_from_java`] hands a pointer to
//! `ndk_sys::AAssetManager`; the resulting backend
//! opens assets under `<key>` and `plugin-<id>/<key>` — matching the
//! layout `emit_app` writes into `android/app/src/main/assets/`.

#[cfg(target_os = "android")]
mod imp {
    use std::ffi::CString;
    use std::io::{self, Read, Seek, SeekFrom};
    use std::ptr::NonNull;
    use std::sync::Arc;

    use istmo_core::assets::{AssetBackend, AssetReader, AssetScope, install_backend};
    use jni::JNIEnv;
    use jni::objects::{GlobalRef, JObject};

    /// `SEEK_SET` (see `<unistd.h>`) — POSIX absolute offset.
    const SEEK_SET: i32 = 0;
    /// `SEEK_CUR` — offset relative to current position.
    const SEEK_CUR: i32 = 1;
    /// `SEEK_END` — offset relative to end of file.
    const SEEK_END: i32 = 2;
    /// `AASSET_MODE_STREAMING` — sequential read, no full-file
    /// mmap. Fine for both `read_to_end` and short random reads that
    /// still support seek.
    const AASSET_MODE_STREAMING: i32 = 2;

    /// [`AssetBackend`] backed by an `AAssetManager*`.
    pub(super) struct AndroidAssetBackend {
        manager: SendPtr,
        /// The NDK documents that the `AAssetManager*` returned by
        /// `AAssetManager_fromJava` is only valid while the Java
        /// `AssetManager` it came from is alive. The global reference
        /// pins it for as long as the backend is installed.
        _java_manager: GlobalRef,
    }

    #[derive(Clone, Copy)]
    struct SendPtr(NonNull<ndk_sys::AAssetManager>);

    // SAFETY: `AAssetManager*` is thread-safe — the NDK explicitly
    // supports concurrent `AAssetManager_open` from multiple threads,
    // and the pointer itself carries no thread affinity.
    unsafe impl Send for SendPtr {}
    // SAFETY: as above; every use goes through `&self` and the NDK
    // serialises access internally.
    unsafe impl Sync for SendPtr {}

    impl std::fmt::Debug for AndroidAssetBackend {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("AndroidAssetBackend")
                .finish_non_exhaustive()
        }
    }

    impl AssetBackend for AndroidAssetBackend {
        fn open(&self, scope: AssetScope<'_>, key: &str) -> io::Result<AssetReader> {
            let candidates: Vec<String> = match scope {
                AssetScope::App => vec![key.to_owned()],
                AssetScope::Plugin(id) => istmo_core::assets::plugin_asset_dirs(id)
                    .iter()
                    .map(|dir| format!("{dir}/{key}"))
                    .collect(),
            };
            for full in &candidates {
                let cstring = CString::new(full.as_str()).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "asset key contains NUL byte")
                })?;
                // SAFETY: `manager` came from `AAssetManager_fromJava` and
                // stays valid because `_java_manager` keeps its Java peer
                // alive; `cstring` is a nul-terminated buffer we own for the
                // duration of the call.
                let asset_ptr = unsafe {
                    ndk_sys::AAssetManager_open(
                        self.manager.0.as_ptr(),
                        cstring.as_ptr(),
                        AASSET_MODE_STREAMING,
                    )
                };
                if let Some(asset) = NonNull::new(asset_ptr) {
                    return Ok(AssetReader::new(AAssetReader { asset }));
                }
            }
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("android asset not found: {}", candidates[0]),
            ))
        }
    }

    /// Owning wrapper around an `AAsset*` — closes on drop, forwards
    /// `Read` / `Seek` through the NDK.
    ///
    /// The `AAsset` keeps its own reference to the asset data, but it is
    /// only valid while its `AAssetManager` is; readers outliving the
    /// installed backend are fine because the backend is process-global
    /// and never uninstalled.
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
            // wrapper; `buf` is a valid mutable slice of `buf.len()`
            // bytes.
            let n = unsafe {
                ndk_sys::AAsset_read(self.asset.as_ptr(), buf.as_mut_ptr().cast(), buf.len())
            };
            usize::try_from(n).map_err(|_| io::Error::other("AAsset_read returned negative"))
        }
    }

    impl Seek for AAssetReader {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            let (offset, whence) = match pos {
                SeekFrom::Start(off) => (
                    i64::try_from(off).map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidInput, "seek offset exceeds i64")
                    })?,
                    SEEK_SET,
                ),
                SeekFrom::End(off) => (off, SEEK_END),
                SeekFrom::Current(off) => (off, SEEK_CUR),
            };
            // SAFETY: `self.asset` is a valid `AAsset*` owned by this
            // wrapper.
            let result = unsafe { ndk_sys::AAsset_seek64(self.asset.as_ptr(), offset, whence) };
            u64::try_from(result)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "AAsset_seek64 failed"))
        }
    }

    impl Drop for AAssetReader {
        fn drop(&mut self) {
            // SAFETY: `self.asset` is a valid `AAsset*` obtained from
            // `AAssetManager_open` and closed exactly once, here.
            unsafe { ndk_sys::AAsset_close(self.asset.as_ptr()) };
        }
    }

    /// Install the backend from a Java-side `AssetManager` reference.
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] with [`io::ErrorKind::InvalidInput`] when
    /// `AAssetManager_fromJava` returns null, or
    /// [`io::ErrorKind::Other`] when the global reference cannot be
    /// created.
    pub fn install_from_java(env: &mut JNIEnv, assets: &JObject) -> io::Result<()> {
        let java_manager = env
            .new_global_ref(assets)
            .map_err(|err| io::Error::other(format!("AssetManager global ref: {err}")))?;
        // SAFETY: `env.get_raw()` yields the valid `JNIEnv*` of the
        // current thread; `java_manager` is a live global reference to
        // the `AssetManager`, valid for the call and beyond.
        let raw = unsafe {
            ndk_sys::AAssetManager_fromJava(
                env.get_raw().cast(),
                java_manager.as_obj().as_raw().cast(),
            )
        };
        let ptr = NonNull::new(raw).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "AAssetManager_fromJava returned null",
            )
        })?;
        install_backend(Arc::new(AndroidAssetBackend {
            manager: SendPtr(ptr),
            _java_manager: java_manager,
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

    #[allow(
        clippy::missing_const_for_fn,
        reason = "the Android implementation of the same signature is not const"
    )]
    pub fn install_from_java(_env: &mut JNIEnv, _assets: &JObject) -> io::Result<()> {
        Ok(())
    }
}

pub use imp::install_from_java;
