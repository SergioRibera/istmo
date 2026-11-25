//! Platform buffers Kotlin hands to Rust frame sinks.
//!
//! Two backings land in a [`NativeBuffer`]:
//!
//! - [`HardwareBuffer`]: an `AHardwareBuffer` Rust holds its own
//!   reference to (`AHardwareBuffer_acquire`), released on drop. Kotlin
//!   may close its `android.hardware.HardwareBuffer` / `Image` right
//!   after submitting.
//! - [`DirectByteBuffer`]: the memory of a direct `java.nio.ByteBuffer`,
//!   kept alive by a JNI global reference, read in place.
//!
//! The NDK `AHardwareBuffer_*` functions are API 26; the library's
//! minimum is lower, so they are looked up with `dlsym` on first use
//! instead of being linked (a missing linked symbol would stop the whole
//! library from loading on an older device).

use std::ffi::c_void;
use std::fmt;
use std::ptr::NonNull;

use istmo_core::native_frame::NativeBuffer;
use jni::objects::GlobalRef;

/// What `AHardwareBuffer_describe` reported when the buffer arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HardwareBufferDesc {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Number of image layers.
    pub layers: u32,
    /// `AHARDWAREBUFFER_FORMAT_*`.
    pub format: u32,
    /// `AHARDWAREBUFFER_USAGE_*` flags.
    pub usage: u64,
    /// Row stride in pixels.
    pub stride: u32,
}

/// Releases the reference a [`HardwareBuffer`] holds.
type ReleaseFn = unsafe extern "C" fn(*mut c_void);

/// An `AHardwareBuffer` Rust holds one reference to.
///
/// Use [`as_ptr`](Self::as_ptr) with the NDK (`AHardwareBuffer_lock`,
/// `eglGetNativeClientBufferANDROID`, `VK_ANDROID_external_memory_android_hardware_buffer`,
/// `AMediaCodec` input) while the value lives.
pub struct HardwareBuffer {
    ptr: NonNull<c_void>,
    desc: HardwareBufferDesc,
    release: ReleaseFn,
}

// SAFETY: an `AHardwareBuffer` is a reference-counted, thread-safe NDK
// object: acquiring, describing and releasing it are allowed from any
// thread, and this value owns exactly one reference.
unsafe impl Send for HardwareBuffer {}
// SAFETY: as above; `&HardwareBuffer` only hands out the pointer and a
// copied description.
unsafe impl Sync for HardwareBuffer {}

impl HardwareBuffer {
    /// The `AHardwareBuffer*`, valid while `self` lives.
    #[must_use]
    pub const fn as_ptr(&self) -> *mut c_void {
        self.ptr.as_ptr()
    }

    /// Size, format and usage of the buffer.
    #[must_use]
    pub const fn desc(&self) -> HardwareBufferDesc {
        self.desc
    }
}

impl Drop for HardwareBuffer {
    fn drop(&mut self) {
        // SAFETY: `ptr` holds the one reference this value acquired, and
        // `release` is `AHardwareBuffer_release`.
        unsafe { (self.release)(self.ptr.as_ptr()) };
    }
}

impl fmt::Debug for HardwareBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HardwareBuffer")
            .field("ptr", &self.ptr)
            .field("desc", &self.desc)
            .finish_non_exhaustive()
    }
}

/// The memory of a direct `java.nio.ByteBuffer`, read in place.
///
/// Kotlin must not write the buffer once it submitted it: Rust reads it
/// until the frame drops.
pub struct DirectByteBuffer {
    // Keeps the `ByteBuffer` (and so its memory) alive.
    _buffer: GlobalRef,
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: the memory belongs to a direct buffer pinned by a JNI global
// reference (itself `Send + Sync`); nothing here mutates it.
unsafe impl Send for DirectByteBuffer {}
// SAFETY: as above: shared access is read-only.
unsafe impl Sync for DirectByteBuffer {}

impl DirectByteBuffer {
    /// The first `len` bytes of the direct buffer at `ptr`, kept alive
    /// by `buffer`.
    ///
    /// # Safety
    ///
    /// `ptr` must be the address `GetDirectBufferAddress` returned for
    /// the buffer `buffer` refers to, and `len` at most its capacity.
    pub(crate) const unsafe fn new(buffer: GlobalRef, ptr: NonNull<u8>, len: usize) -> Self {
        Self {
            _buffer: buffer,
            ptr,
            len,
        }
    }
}

impl AsRef<[u8]> for DirectByteBuffer {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: `new`'s contract: `ptr` addresses at least `len` bytes
        // of a direct buffer kept alive by `_buffer`.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl fmt::Debug for DirectByteBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirectByteBuffer")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

/// Wrap the `android.hardware.HardwareBuffer` `object` in a
/// [`NativeBuffer`], taking a reference of Rust's own. `None` when the
/// NDK functions are missing (below API 26) or the object is no buffer.
#[cfg(target_os = "android")]
pub(crate) fn hardware_buffer(
    env: &jni::JNIEnv<'_>,
    object: &jni::objects::JObject<'_>,
) -> Option<NativeBuffer> {
    let api = ndk::api()?;
    // SAFETY: `env` is the calling thread's live JNI environment and
    // `object` a local reference valid for this call;
    // `AHardwareBuffer_fromHardwareBuffer` borrows the native buffer the
    // Java object wraps (or returns null for anything else).
    let ptr = unsafe { (api.from_java)(env.get_raw(), object.as_raw()) };
    let ptr = NonNull::new(ptr)?;
    let mut raw = ndk::RawDesc::default();
    // SAFETY: `ptr` is a live buffer (the Java object holds it for the
    // duration of this call); acquiring takes Rust's own reference before
    // the Java side may release its own; `describe` fills `raw`.
    unsafe {
        (api.acquire)(ptr.as_ptr());
        (api.describe)(ptr.as_ptr(), &raw mut raw);
    }
    let buffer = HardwareBuffer {
        ptr,
        desc: HardwareBufferDesc {
            width: raw.width,
            height: raw.height,
            layers: raw.layers,
            format: raw.format,
            usage: raw.usage,
            stride: raw.stride,
        },
        release: api.release,
    };
    Some(NativeBuffer::new(
        istmo_core::native_frame::MemoryKind::HardwareBuffer,
        buffer,
    ))
}

/// `HardwareBuffer` needs Android.
#[cfg(not(target_os = "android"))]
pub(crate) const fn hardware_buffer(
    _env: &jni::JNIEnv<'_>,
    _object: &jni::objects::JObject<'_>,
) -> Option<NativeBuffer> {
    None
}

/// The NDK functions, resolved at run time.
#[cfg(target_os = "android")]
mod ndk {
    use std::ffi::{CStr, c_char, c_int, c_void};
    use std::sync::OnceLock;

    use super::ReleaseFn;

    /// `AHardwareBuffer_Desc`.
    #[repr(C)]
    #[derive(Default)]
    pub(super) struct RawDesc {
        pub(super) width: u32,
        pub(super) height: u32,
        pub(super) layers: u32,
        pub(super) format: u32,
        pub(super) usage: u64,
        pub(super) stride: u32,
        rfu0: u32,
        rfu1: u64,
    }

    pub(super) struct Api {
        pub(super) from_java:
            unsafe extern "C" fn(*mut jni::sys::JNIEnv, jni::sys::jobject) -> *mut c_void,
        pub(super) acquire: unsafe extern "C" fn(*mut c_void),
        pub(super) release: ReleaseFn,
        pub(super) describe: unsafe extern "C" fn(*const c_void, *mut RawDesc),
    }

    const RTLD_NOW: c_int = 2;

    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    /// `None` below API 26.
    pub(super) fn api() -> Option<&'static Api> {
        static API: OnceLock<Option<Api>> = OnceLock::new();
        API.get_or_init(resolve).as_ref()
    }

    fn resolve() -> Option<Api> {
        // SAFETY: a NUL-terminated library name; `libandroid.so` is
        // already loaded in every app process, so this only bumps its
        // reference count (never unloaded: the handle is kept forever).
        let lib = unsafe { dlopen(c"libandroid.so".as_ptr(), RTLD_NOW) };
        if lib.is_null() {
            tracing::warn!("libandroid.so not found; HardwareBuffer frames unsupported");
            return None;
        }
        let symbol = |name: &CStr| {
            // SAFETY: `lib` is a live handle and `name` NUL-terminated.
            let ptr = unsafe { dlsym(lib, name.as_ptr()) };
            (!ptr.is_null()).then_some(ptr)
        };
        let from_java = symbol(c"AHardwareBuffer_fromHardwareBuffer")?;
        let acquire = symbol(c"AHardwareBuffer_acquire")?;
        let release = symbol(c"AHardwareBuffer_release")?;
        let describe = symbol(c"AHardwareBuffer_describe")?;
        // SAFETY: each symbol is the NDK function of that name, whose C
        // signature (`<android/hardware_buffer_jni.h>`,
        // `<android/hardware_buffer.h>`) is the one transmuted to.
        unsafe {
            Some(Api {
                from_java: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(*mut jni::sys::JNIEnv, jni::sys::jobject) -> *mut c_void,
                >(from_java),
                acquire: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut c_void)>(
                    acquire,
                ),
                release: std::mem::transmute::<*mut c_void, ReleaseFn>(release),
                describe: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(*const c_void, *mut RawDesc),
                >(describe),
            })
        }
    }
}
