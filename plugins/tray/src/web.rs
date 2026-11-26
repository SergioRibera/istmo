//! Web: the tab's favicon. One tray per document; only the icon is
//! supported. Images go to the browser as Blob URLs, undecoded;
//! animations run on `setTimeout`, which browsers slow to about once a
//! second in background tabs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use istmo_core::NativeHandleId;
use istmo_core::sync::lock;
use istmo_menu::{IconImage, ImageData};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::host::{Backend, Dirty, Host, Shared, delegate_tray};
use crate::{
    HostState, ImageError, ImageFailure, LimitKind, Repeat, TrayBackend, TrayCapabilities,
    TrayError,
};

/// Most frames an animation may have.
pub(crate) const MAX_FRAMES: usize = 512;
/// Largest image accepted.
const MAX_IMAGE_BYTES: usize = 8 << 20;
/// Shortest frame the browser is asked to show.
const MIN_FRAME_MS: u32 = 16;

/// An icon the browser decodes: the first SVG / PNG / JPEG of the image.
#[derive(Debug)]
pub(crate) struct DecodedIcon {
    mime: &'static str,
    bytes: Vec<u8>,
    url: OnceLock<String>,
}

impl DecodedIcon {
    pub(crate) fn decode(image: &IconImage, frame: u32, _names: bool) -> Result<Self, TrayError> {
        for (at, data) in image.reps.iter().enumerate() {
            let (mime, bytes) = match data {
                ImageData::Svg(bytes) => ("image/svg+xml", bytes),
                ImageData::Png(bytes) => ("image/png", bytes),
                ImageData::Jpeg(bytes) => ("image/jpeg", bytes),
                // Platform names, and formats added to the model later.
                _ => continue,
            };
            if bytes.len() > MAX_IMAGE_BYTES {
                return Err(TrayError::Limit(LimitKind::ImageBytes));
            }
            if bytes.is_empty() {
                return Err(TrayError::Image(ImageError {
                    frame,
                    rep: u32::try_from(at).unwrap_or(u32::MAX),
                    reason: ImageFailure::Empty,
                }));
            }
            return Ok(Self {
                mime,
                bytes: bytes.clone(),
                url: OnceLock::new(),
            });
        }
        Err(TrayError::Image(ImageError {
            frame,
            rep: 0,
            reason: ImageFailure::NothingUsable,
        }))
    }

    /// A Blob URL for the image, made on first use.
    fn url(&self) -> Option<&str> {
        if let Some(url) = self.url.get() {
            return Some(url);
        }
        let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(self.bytes.as_slice()));
        let options = web_sys::BlobPropertyBag::new();
        options.set_type(self.mime);
        let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options).ok()?;
        let url = web_sys::Url::create_object_url_with_blob(&blob).ok()?;
        Some(self.url.get_or_init(|| url))
    }
}

impl Drop for DecodedIcon {
    fn drop(&mut self) {
        if let Some(url) = self.url.get() {
            let _revoked = web_sys::Url::revoke_object_url(url);
        }
    }
}

/// The page's favicon before the tray replaced it.
#[derive(Debug)]
enum Original {
    Href(String),
    Absent,
}

/// The favicon's state.
#[derive(Debug, Default)]
struct Favicon {
    /// The page's own favicon, restored when the tray goes away; `None`
    /// until the tray first shows an icon.
    original: Option<Original>,
    /// Bumped on every icon change: older timers stop.
    generation: HashMap<NativeHandleId, u64>,
}

struct Web {
    shared: Weak<Shared>,
    favicon: Arc<Mutex<Favicon>>,
}

impl Backend for Web {
    fn capabilities(&self) -> TrayCapabilities {
        TrayCapabilities {
            max_items: Some(1),
            min_frame_ms: MIN_FRAME_MS,
            host: HostState::Available,
            ..TrayCapabilities::none(TrayBackend::Favicon)
        }
    }

    fn wake(&self) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        for (tray, dirty) in shared.take_pending() {
            apply(&self.shared, &self.favicon, tray, &dirty);
        }
    }
}

#[expect(clippy::unnecessary_wraps, reason = "has the `StartBackend` signature")]
fn start(shared: &Arc<Shared>) -> Result<Arc<dyn Backend>, TrayError> {
    shared.set_host(HostState::Available);
    Ok(Arc::new(Web {
        shared: Arc::downgrade(shared),
        favicon: Arc::default(),
    }))
}

fn link() -> Option<web_sys::Element> {
    let document = web_sys::window()?.document()?;
    if let Ok(Some(link)) = document.query_selector("link[rel~='icon']") {
        return Some(link);
    }
    let link = document.create_element("link").ok()?;
    link.set_attribute("rel", "icon").ok()?;
    document.head()?.append_child(&link).ok()?;
    Some(link)
}

fn show(icon: &DecodedIcon) {
    let (Some(link), Some(url)) = (link(), icon.url()) else {
        return;
    };
    let _typed = link.set_attribute("type", icon.mime);
    let _set = link.set_attribute("href", url);
}

fn apply(
    shared: &Weak<Shared>,
    favicon: &Arc<Mutex<Favicon>>,
    tray: NativeHandleId,
    dirty: &Dirty,
) {
    if dirty.removed {
        let original = {
            let mut state = lock(favicon);
            state.generation.remove(&tray);
            state.original.take()
        };
        if let (Some(original), Some(link)) = (original, link()) {
            let _restored = match original {
                Original::Href(href) => link.set_attribute("href", &href),
                Original::Absent => link.remove_attribute("href"),
            };
        }
        if !dirty.added {
            return;
        }
    }
    if !(dirty.added || dirty.icon) {
        return;
    }
    let current = link().and_then(|l| l.get_attribute("href"));
    let generation = {
        let mut state = lock(favicon);
        if state.original.is_none() {
            state.original = Some(current.map_or(Original::Absent, Original::Href));
        }
        let generation = state.generation.entry(tray).or_default();
        *generation += 1;
        let generation = *generation;
        drop(state);
        generation
    };
    step(shared.clone(), Arc::clone(favicon), tray, generation, 0, 0);
}

/// Shows frame `frame` and schedules the next one.
fn step(
    shared: Weak<Shared>,
    favicon: Arc<Mutex<Favicon>>,
    tray: NativeHandleId,
    generation: u64,
    frame: usize,
    runs: u32,
) {
    if lock(&favicon).generation.get(&tray) != Some(&generation) {
        return;
    }
    let Some(strong) = shared.upgrade() else {
        return;
    };
    let next = {
        let state = strong.lock();
        let Some(entry) = state.trays.get(&tray) else {
            return;
        };
        let frames = &entry.icon.frames;
        let Some((icon, duration)) = frames.get(frame) else {
            return;
        };
        show(icon);
        let (next, runs) = if frame + 1 < frames.len() {
            (frame + 1, runs)
        } else {
            (0, runs.saturating_add(1))
        };
        let done = frames.len() < 2
            || match entry.icon.repeat {
                Repeat::Forever => false,
                Repeat::Times(times) => runs >= times,
            };
        let duration = *duration;
        drop(state);
        (!done).then_some((next, runs, duration))
    };
    let Some((next, runs, duration)) = next else {
        return;
    };
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback =
        Closure::once_into_js(move || step(shared, favicon, tray, generation, next, runs));
    let millis = i32::try_from(duration.as_millis()).unwrap_or(i32::MAX);
    let _timer = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), millis);
}

/// The tray host for the web: the tab's favicon. Menus, titles, tooltips
/// and visibility are accepted and ignored, so code written for the
/// desktop runs unchanged; `capabilities()` reports them unsupported.
#[derive(Debug)]
pub struct WebTray {
    host: Host,
}

impl Default for WebTray {
    fn default() -> Self {
        Self {
            host: Host::new(start),
        }
    }
}

delegate_tray!(WebTray);
