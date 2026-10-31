//! Pure-Rust desktop sampler for
//! [`super::network_state::NetworkStateClient`].
//!
//! - Linux inspects `/proc/net/route` to detect a default route and
//!   scans `/sys/class/net/*/wireless` for the active interface's
//!   link kind.
//! - Windows calls `GetBestInterface` + `GetAdaptersInfo` via
//!   `windows-sys` to classify the active adapter.
//! - macOS and BSDs are not covered — plug the native observer
//!   (`NWPathMonitor` Swift bridge) or publish bytes directly via
//!   `Runtime::publish_early_latest`.

use std::sync::{Arc, Weak};

use istmo_core::{IstmoError, Runtime, codec};

use super::network_state::{
    LinkKind, NETWORK_STATE_CHANNEL, NETWORK_STATE_PLUGIN_ID, NetworkState,
};

#[derive(Debug)]
pub struct NetworkStatePublisher {
    runtime: Weak<Runtime>,
    last: Option<NetworkState>,
}

impl NetworkStatePublisher {
    pub fn install(runtime: &Arc<Runtime>) -> Result<Self, IstmoError> {
        runtime.check_declared(NETWORK_STATE_PLUGIN_ID)?;
        Ok(Self {
            runtime: Arc::downgrade(runtime),
            last: None,
        })
    }

    pub fn tick(&mut self) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let Some(sample) = sample() else {
            return;
        };
        if self.last.as_ref() == Some(&sample) {
            return;
        }
        match codec::encode(&sample) {
            Ok(bytes) => {
                runtime.publish_early_latest(NETWORK_STATE_CHANNEL, bytes);
                self.last = Some(sample);
            }
            Err(err) => tracing::warn!(target = "istmo.network_state", ?err, "encode failed"),
        }
    }
}

fn sample() -> Option<NetworkState> {
    #[cfg(target_os = "linux")]
    {
        linux::sample()
    }
    #[cfg(target_os = "windows")]
    {
        windows::sample()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;

    use super::{LinkKind, NetworkState};

    #[allow(clippy::unnecessary_wraps)]
    pub(super) fn sample() -> Option<NetworkState> {
        let default_iface = default_route_interface();
        let online = default_iface.is_some();
        let link = default_iface
            .as_deref()
            .map_or(LinkKind::None, classify_link);
        Some(NetworkState {
            online,
            link,
            metered: false,
            expensive: false,
            constrained: false,
        })
    }

    fn default_route_interface() -> Option<String> {
        let routes = fs::read_to_string("/proc/net/route").ok()?;
        for line in routes.lines().skip(1) {
            let mut cols = line.split_whitespace();
            let iface = cols.next()?;
            let dest = cols.next()?;
            let _gw = cols.next()?;
            let flags = cols.next()?;
            // Default route: destination = 00000000 and flag bit 0x1 (UP).
            if dest == "00000000"
                && u32::from_str_radix(flags, 16)
                    .ok()
                    .is_some_and(|f| f & 1 != 0)
            {
                return Some(iface.to_owned());
            }
        }
        None
    }

    fn classify_link(iface: &str) -> LinkKind {
        if fs::metadata(format!("/sys/class/net/{iface}/wireless")).is_ok() {
            return LinkKind::Wifi;
        }
        let kind_path = format!("/sys/class/net/{iface}/type");
        if let Ok(raw) = fs::read_to_string(&kind_path) {
            // ARPHRD_* numeric codes — 1 = ethernet, 772 = loopback,
            // 820 = ppp, 823 = ieee80211 (also seen for cellular on
            // some stacks). 24 covers some tunnel/VPN interfaces.
            if let Ok(code) = raw.trim().parse::<u32>() {
                return match code {
                    1 => LinkKind::Ethernet,
                    768 | 823 => LinkKind::Wifi,
                    _ if iface.starts_with("ppp")
                        || iface.starts_with("wwan")
                        || iface.starts_with("rmnet") =>
                    {
                        LinkKind::Cellular
                    }
                    _ if iface.starts_with("tun") || iface.starts_with("tap") => LinkKind::Vpn,
                    _ => LinkKind::Other,
                };
            }
        }
        LinkKind::Other
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows {
    use std::mem;
    use std::ptr;

    use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211, IF_TYPE_PPP,
        IF_TYPE_TUNNEL, IF_TYPE_WWANPP, IF_TYPE_WWANPP2, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::Networking::WinSock::AF_UNSPEC;

    use super::{LinkKind, NetworkState};

    const GAA_FLAG_INCLUDE_PREFIX: u32 = 0x0010;
    const IF_OPER_STATUS_UP: u32 = 1;

    pub(super) fn sample() -> Option<NetworkState> {
        let mut size: u32 = 15 * 1024;
        let mut buf: Vec<u8> = Vec::with_capacity(size as usize);
        // SAFETY: `GetAdaptersAddresses` fills `buf` up to `size` and
        // reports `ERROR_BUFFER_OVERFLOW` when more is needed.
        loop {
            let rc = unsafe {
                GetAdaptersAddresses(
                    u32::from(AF_UNSPEC),
                    GAA_FLAG_INCLUDE_PREFIX,
                    ptr::null(),
                    buf.as_mut_ptr().cast(),
                    &raw mut size,
                )
            };
            match rc {
                NO_ERROR => break,
                ERROR_BUFFER_OVERFLOW => {
                    buf = Vec::with_capacity(size as usize);
                    continue;
                }
                _ => return None,
            }
        }

        let mut link = LinkKind::None;
        let mut online = false;

        let mut ptr_cur = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        while !ptr_cur.is_null() {
            // SAFETY: pointer walk through linked list filled by Win32.
            let adapter = unsafe { &*ptr_cur };
            if adapter.OperStatus == IF_OPER_STATUS_UP {
                online = true;
                let adapter_link = match adapter.IfType {
                    IF_TYPE_IEEE80211 => LinkKind::Wifi,
                    IF_TYPE_ETHERNET_CSMACD => LinkKind::Ethernet,
                    IF_TYPE_PPP | IF_TYPE_WWANPP | IF_TYPE_WWANPP2 => LinkKind::Cellular,
                    IF_TYPE_TUNNEL => LinkKind::Vpn,
                    _ => LinkKind::Other,
                };
                // Prefer Wi-Fi over Ethernet when both are up (laptops
                // often have both when docked but Wi-Fi is the
                // user-visible one).
                link = match (link, adapter_link) {
                    (LinkKind::None, kind) => kind,
                    (LinkKind::Ethernet, LinkKind::Wifi) => LinkKind::Wifi,
                    (existing, _) => existing,
                };
            }
            ptr_cur = adapter.Next;
        }

        // Prevent "unused mem" import pedantic lint on 32-bit builds.
        let _ = mem::size_of::<IP_ADAPTER_ADDRESSES_LH>();

        Some(NetworkState {
            online,
            link,
            metered: false,
            expensive: false,
            constrained: false,
        })
    }
}
