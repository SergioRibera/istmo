use istmo_core::{HostCtx, NativeHandleId};
use istmo_menu::{IconImage, ItemPatch, MenuNode};

use crate::{
    EventSupport, Feature, HostState, MenuSupport, RegisteredIcon, Support, Tooltip, Tray,
    TrayBackend, TrayCapabilities, TrayError, TrayEvent, TrayIcon, TrayItem, TraySpec,
};

/// Host for platforms without a tray (Android, iOS): every call answers
/// [`TrayError::Unsupported`].
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableTray;

impl TrayCapabilities {
    /// A report where nothing is supported.
    #[must_use]
    pub const fn none(backend: TrayBackend) -> Self {
        Self {
            backend,
            host: HostState::Missing,
            max_items: Some(0),
            min_frame_ms: 0,
            image_names: Support::Unsupported,
            template_images: Support::Unsupported,
            title: Support::Unsupported,
            tooltip_body: Support::Unsupported,
            menu: MenuSupport::NONE,
            events: EventSupport::NONE,
            anchor: None,
        }
    }
}

impl MenuSupport {
    /// No menu at all.
    pub const NONE: Self = Self {
        check: Support::Unsupported,
        mixed: Support::Unsupported,
        radio: Support::Unsupported,
        item_icons: Support::Unsupported,
        shortcuts: Support::Unsupported,
        header: Support::Unsupported,
        subtitle: Support::Unsupported,
        badge: Support::Unsupported,
        default_item: Support::Unsupported,
        mnemonics: Support::Unsupported,
        live_while_open: Support::Unsupported,
    };
}

impl EventSupport {
    /// No events at all.
    pub const NONE: Self = Self {
        activate: Support::Unsupported,
        secondary: Support::Unsupported,
        double_click: Support::Unsupported,
        scroll: Support::Unsupported,
        menu_open_close: Support::Unsupported,
    };
}

const UNSUPPORTED: TrayError = TrayError::Unsupported(Feature::Tray);

impl Tray for UnavailableTray {
    async fn capabilities(&self) -> TrayCapabilities {
        TrayCapabilities::none(TrayBackend::Unavailable)
    }

    async fn register_icon(
        &self,
        _ctx: HostCtx,
        _image: IconImage,
    ) -> Result<RegisteredIcon, TrayError> {
        Err(UNSUPPORTED)
    }

    async fn create(&self, _ctx: HostCtx, _spec: TraySpec) -> Result<TrayItem, TrayError> {
        Err(UNSUPPORTED)
    }

    async fn set_icon(&self, _tray: NativeHandleId, _icon: TrayIcon) -> Result<(), TrayError> {
        Err(UNSUPPORTED)
    }

    async fn set_title(
        &self,
        _tray: NativeHandleId,
        _title: Option<String>,
    ) -> Result<(), TrayError> {
        Err(UNSUPPORTED)
    }

    async fn set_tooltip(
        &self,
        _tray: NativeHandleId,
        _tooltip: Option<Tooltip>,
    ) -> Result<(), TrayError> {
        Err(UNSUPPORTED)
    }

    async fn set_visible(&self, _tray: NativeHandleId, _visible: bool) -> Result<(), TrayError> {
        Err(UNSUPPORTED)
    }

    async fn set_menu(&self, _tray: NativeHandleId, _menu: Vec<MenuNode>) -> Result<(), TrayError> {
        Err(UNSUPPORTED)
    }

    async fn update_items(
        &self,
        _tray: NativeHandleId,
        _patches: Vec<ItemPatch>,
    ) -> Result<(), TrayError> {
        Err(UNSUPPORTED)
    }

    async fn destroy(&self, _tray: NativeHandleId) -> Result<(), TrayError> {
        Err(UNSUPPORTED)
    }

    fn events(&self, _tray: NativeHandleId) -> flume::Receiver<Result<TrayEvent, TrayError>> {
        let (tx, rx) = flume::bounded(1);
        let _queued = tx.try_send(Err(UNSUPPORTED));
        rx
    }
}
