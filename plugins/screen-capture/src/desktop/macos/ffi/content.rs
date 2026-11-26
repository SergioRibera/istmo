//! `SCShareableContent` (displays, windows, applications) and the
//! `SCContentFilter`s built from it.

use std::ptr::NonNull;
use std::time::Duration;

use block2::RcBlock;
use objc2::AllocAnyThread as _;
use objc2::rc::Retained;
use objc2_core_graphics::{
    CGDisplayCopyDisplayMode, CGDisplayIsMain, CGDisplayMode, CGDisplayModelNumber,
    CGDisplayVendorNumber,
};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCRunningApplication, SCShareableContent, SCShareableContentStyle,
    SCWindow,
};

use crate::types::{CaptureError, OsVersion, Size};

use super::super::super::sck_map::{self, RectF};
use super::{Shared, error_of, wait};

/// How long listing may take (the window server answers in
/// milliseconds; a first call can wait on the TCC daemon).
const LIST_TIMEOUT: Duration = Duration::from_secs(5);

/// One display as listed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DisplayInfo {
    /// `CGDirectDisplayID`.
    pub(crate) id: u32,
    /// Global desktop coordinates, points.
    pub(crate) frame: RectF,
    /// The current mode's pixel size.
    pub(crate) pixels: Option<Size>,
    pub(crate) refresh_hz: Option<f64>,
    pub(crate) main: bool,
    pub(crate) vendor: u32,
    pub(crate) model: u32,
}

/// One application as listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppInfo {
    pub(crate) bundle_id: String,
    pub(crate) name: String,
    pub(crate) pid: i32,
}

/// One window as listed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WindowInfo {
    /// `CGWindowID`.
    pub(crate) id: u32,
    /// Global desktop coordinates, points.
    pub(crate) frame: RectF,
    pub(crate) title: Option<String>,
    /// `windowLayer`: 0 for normal application windows.
    pub(crate) layer: isize,
    pub(crate) on_screen: bool,
    /// `isActive` (13.1+); `None` before.
    pub(crate) active: Option<bool>,
    pub(crate) app: Option<AppInfo>,
}

/// What a filter leaves out of a display.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Exclusions {
    /// Applications by bundle id.
    pub(crate) bundle_ids: Vec<String>,
    /// Applications by process id (this process, apps without a bundle).
    pub(crate) pids: Vec<i32>,
    /// Windows by `CGWindowID`.
    pub(crate) windows: Vec<u32>,
}

/// A snapshot of what can be captured. Listing needs the Screen
/// Recording grant; callers check it first so this never prompts.
#[derive(Debug)]
pub(crate) struct Content(Shared<SCShareableContent>);

impl Content {
    /// The current displays, windows and applications.
    ///
    /// # Errors
    /// The framework refused (no grant: `UserDeclined`) or did not answer.
    pub(crate) fn fetch() -> Result<Self, CaptureError> {
        let (tx, rx) = flume::bounded(1);
        let block = RcBlock::new(
            move |content: *mut SCShareableContent, error: *mut NSError| {
                let answer = NonNull::new(content).map_or_else(
                    || {
                        // SAFETY: the framework passes null or a valid error
                        // for the duration of the call.
                        let error = unsafe { error.as_ref() };
                        Err(error.map_or_else(
                            || {
                                CaptureError::protocol(
                                    sck_map::ID,
                                    "sources",
                                    "no content and no error",
                                )
                            },
                            |error| error_of(error, "sources"),
                        ))
                    },
                    |content| {
                        // SAFETY: the framework passes a valid object for the
                        // duration of the call; retaining keeps it past it.
                        unsafe { Retained::retain(content.as_ptr()) }
                            .map(|content| Self(Shared(content)))
                            .ok_or_else(|| {
                                CaptureError::protocol(sck_map::ID, "sources", "no content")
                            })
                    },
                );
                // The caller may have timed out and left.
                let _sent = tx.try_send(answer);
            },
        );
        // SAFETY: the block is a valid completion handler of the declared
        // signature; the framework copies it.
        unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
        wait(&rx, LIST_TIMEOUT, "sources")
    }

    fn displays_raw(&self) -> Retained<NSArray<SCDisplay>> {
        // SAFETY: a getter on a valid snapshot.
        unsafe { self.0.displays() }
    }

    fn windows_raw(&self) -> Retained<NSArray<SCWindow>> {
        // SAFETY: a getter on a valid snapshot.
        unsafe { self.0.windows() }
    }

    fn applications_raw(&self) -> Retained<NSArray<SCRunningApplication>> {
        // SAFETY: a getter on a valid snapshot.
        unsafe { self.0.applications() }
    }

    /// Every display.
    #[must_use]
    pub(crate) fn displays(&self) -> Vec<DisplayInfo> {
        self.displays_raw()
            .iter()
            .map(|d| display_info(&d))
            .collect()
    }

    /// Every window, `os` deciding which properties may be asked.
    #[must_use]
    pub(crate) fn windows(&self, os: OsVersion) -> Vec<WindowInfo> {
        self.windows_raw()
            .iter()
            .map(|w| window_info(&w, os))
            .collect()
    }

    /// Every application.
    #[must_use]
    pub(crate) fn applications(&self) -> Vec<AppInfo> {
        self.applications_raw()
            .iter()
            .map(|a| app_info(&a))
            .collect()
    }

    fn display(&self, id: u32) -> Option<Retained<SCDisplay>> {
        self.displays_raw()
            .iter()
            // SAFETY: a getter on a listed display.
            .find(|d| unsafe { d.displayID() } == id)
    }

    fn windows_with(&self, ids: &[u32]) -> Retained<NSArray<SCWindow>> {
        let windows: Vec<Retained<SCWindow>> = self
            .windows_raw()
            .iter()
            // SAFETY: a getter on a listed window.
            .filter(|w| ids.contains(&unsafe { w.windowID() }))
            .collect();
        NSArray::from_retained_slice(&windows)
    }

    fn applications_matching(
        &self,
        matches: impl Fn(&AppInfo) -> bool,
    ) -> Retained<NSArray<SCRunningApplication>> {
        let apps: Vec<Retained<SCRunningApplication>> = self
            .applications_raw()
            .iter()
            .filter(|a| matches(&app_info(a)))
            .collect();
        NSArray::from_retained_slice(&apps)
    }

    /// Display `id` minus what `exclude` names.
    #[must_use]
    pub(crate) fn display_filter(&self, id: u32, exclude: &Exclusions) -> Option<Filter> {
        let display = self.display(id)?;
        let apps = self.applications_matching(|app| {
            exclude.bundle_ids.contains(&app.bundle_id) || exclude.pids.contains(&app.pid)
        });
        // Windows "excepted" from an exclusion list are left out unless
        // their application is in it: here, exactly the excluded windows.
        let windows = self.windows_with(&exclude.windows);
        // SAFETY: a designated initialiser with listed objects.
        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
                SCContentFilter::alloc(),
                &display,
                &apps,
                &windows,
            )
        };
        Some(Filter(Shared(filter)))
    }

    /// Window `id` alone, wherever it is.
    #[must_use]
    pub(crate) fn window_filter(&self, id: u32) -> Option<Filter> {
        let window = self
            .windows_raw()
            .iter()
            // SAFETY: a getter on a listed window.
            .find(|w| unsafe { w.windowID() } == id)?;
        // SAFETY: a designated initialiser with a listed window.
        let filter = unsafe {
            SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window)
        };
        Some(Filter(Shared(filter)))
    }

    /// Every window of the application `bundle_id` (or `pid`) on display
    /// `display`, minus the excluded windows.
    #[must_use]
    pub(crate) fn application_filter(
        &self,
        bundle_id: Option<&str>,
        pid: Option<i32>,
        display: u32,
        exclude: &Exclusions,
    ) -> Option<Filter> {
        let display = self.display(display)?;
        let apps = self.applications_matching(|app| {
            bundle_id.is_some_and(|id| id == app.bundle_id) || pid.is_some_and(|p| p == app.pid)
        });
        if apps.count() == 0 {
            return None;
        }
        let windows = self.windows_with(&exclude.windows);
        // SAFETY: a designated initialiser with listed objects.
        let filter = unsafe {
            SCContentFilter::initWithDisplay_includingApplications_exceptingWindows(
                SCContentFilter::alloc(),
                &display,
                &apps,
                &windows,
            )
        };
        Some(Filter(Shared(filter)))
    }
}

fn display_info(display: &SCDisplay) -> DisplayInfo {
    // SAFETY: a getter on a listed display.
    let id = unsafe { display.displayID() };
    // SAFETY: as above.
    let frame = unsafe { display.frame() };
    let mode = CGDisplayCopyDisplayMode(id);
    let pixels = mode.as_deref().and_then(|mode| {
        let width = u32::try_from(CGDisplayMode::pixel_width(Some(mode))).ok()?;
        let height = u32::try_from(CGDisplayMode::pixel_height(Some(mode))).ok()?;
        (width > 0 && height > 0).then(|| Size::new(width, height))
    });
    let refresh_hz = mode
        .as_deref()
        .map(|mode| CGDisplayMode::refresh_rate(Some(mode)))
        .filter(|hz| *hz > 0.0);
    DisplayInfo {
        id,
        frame: rect(frame),
        pixels,
        refresh_hz,
        main: CGDisplayIsMain(id),
        vendor: CGDisplayVendorNumber(id),
        model: CGDisplayModelNumber(id),
    }
}

fn app_info(app: &SCRunningApplication) -> AppInfo {
    // SAFETY: a getter on a listed application.
    let bundle_id = unsafe { app.bundleIdentifier() }.to_string();
    // SAFETY: as above.
    let name = unsafe { app.applicationName() }.to_string();
    // SAFETY: as above.
    let pid = unsafe { app.processID() };
    AppInfo {
        bundle_id,
        name,
        pid,
    }
}

fn window_info(window: &SCWindow, os: OsVersion) -> WindowInfo {
    // SAFETY: a getter on a listed window.
    let id = unsafe { window.windowID() };
    // SAFETY: as above.
    let frame = rect(unsafe { window.frame() });
    // SAFETY: as above.
    let title = unsafe { window.title() }
        .map(|t| t.to_string())
        .filter(|t| !t.is_empty());
    // SAFETY: as above.
    let layer = unsafe { window.windowLayer() };
    // SAFETY: as above.
    let on_screen = unsafe { window.isOnScreen() };
    // SAFETY: as above; `isActive` only where the OS has it (13.1: an
    // unknown selector would raise).
    let active =
        sck_map::at_least(os, sck_map::WINDOW_ACTIVE).then(|| unsafe { window.isActive() });
    // SAFETY: as above.
    let app = unsafe { window.owningApplication() }.map(|app| app_info(&app));
    WindowInfo {
        id,
        frame,
        title,
        layer,
        on_screen,
        active,
        app,
    }
}

const fn rect(rect: objc2_core_foundation::CGRect) -> RectF {
    RectF {
        x: rect.origin.x,
        y: rect.origin.y,
        width: rect.size.width,
        height: rect.size.height,
    }
}

/// What a filter captures (`SCShareableContentStyle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilterStyle {
    Display,
    Window,
    Application,
    Unknown,
}

/// What a filter says about itself (14+).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FilterInfo {
    pub(crate) style: FilterStyle,
    /// Points.
    pub(crate) content_rect: RectF,
    /// Pixels per point.
    pub(crate) scale: f64,
}

/// A content filter: what a stream or a screenshot captures.
#[derive(Debug)]
pub(crate) struct Filter(pub(super) Shared<SCContentFilter>);

impl Filter {
    /// The filter's own description, on macOS 14+ (`None` before: the
    /// properties do not exist).
    #[must_use]
    pub(crate) fn info(&self, os: OsVersion) -> Option<FilterInfo> {
        if !sck_map::at_least(os, sck_map::PICKER) {
            return None;
        }
        // SAFETY: a getter that exists from 14.0, checked above.
        let style = unsafe { self.0.style() };
        // SAFETY: as above.
        let content_rect = unsafe { self.0.contentRect() };
        // SAFETY: as above.
        let scale = unsafe { self.0.pointPixelScale() };
        let style = match style {
            SCShareableContentStyle::Display => FilterStyle::Display,
            SCShareableContentStyle::Window => FilterStyle::Window,
            SCShareableContentStyle::Application => FilterStyle::Application,
            _ => FilterStyle::Unknown,
        };
        Some(FilterInfo {
            style,
            content_rect: rect(content_rect),
            scale: f64::from(scale),
        })
    }

    pub(super) fn raw(&self) -> &SCContentFilter {
        &self.0
    }
}
