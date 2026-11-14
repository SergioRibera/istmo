//! Pure-Rust desktop sampler for
//! [`NetworkStateClient`](super::network_state::NetworkStateClient).
//!
//! - Linux reads `/proc/net/route` for the default route and
//!   classifies its interface from `/sys/class/net/<iface>/`.
//! - Windows walks `GetAdaptersAddresses`: an adapter that is up, has a
//!   gateway and is neither loopback nor a tunnel pseudo-interface makes
//!   the host online, and the best-ranked such adapter names the link.
//! - macOS runs a Network.framework `nw_path_monitor` (the source the
//!   iOS observer uses too) and reads the path it last reported,
//!   including the expensive and constrained (Low Data Mode) flags.

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos", test))]
use super::network_state::LinkKind;
use super::network_state::NetworkState;
use crate::early::LatestPublisher;

/// Publishes the host's connectivity snapshot.
pub type NetworkStatePublisher = LatestPublisher<NetworkState>;

impl LatestPublisher<NetworkState> {
    /// Sample the host and publish if the snapshot changed.
    pub fn tick(&mut self) {
        if let Some(sample) = sample() {
            self.publish_if_changed(sample);
        }
    }
}

#[cfg_attr(
    not(any(target_os = "linux", target_os = "windows", target_os = "macos")),
    expect(
        clippy::missing_const_for_fn,
        reason = "only the unsupported-target body is const"
    )
)]
fn sample() -> Option<NetworkState> {
    #[cfg(target_os = "linux")]
    {
        linux::sample()
    }
    #[cfg(target_os = "windows")]
    {
        windows::sample()
    }
    #[cfg(target_os = "macos")]
    {
        macos::sample()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        None
    }
}

#[cfg(any(target_os = "linux", target_os = "windows", test))]
const fn snapshot(link: Option<LinkKind>) -> NetworkState {
    NetworkState {
        online: link.is_some(),
        link: match link {
            Some(kind) => kind,
            None => LinkKind::None,
        },
        metered: false,
        expensive: false,
        constrained: false,
    }
}

/// Interface carrying the IPv4 default route in a `/proc/net/route`
/// table: destination `00000000` with the `RTF_UP` flag.
#[cfg(any(target_os = "linux", test))]
fn default_route_interface(table: &str) -> Option<&str> {
    const RTF_UP: u32 = 0x1;
    table.lines().skip(1).find_map(|line| {
        let mut cols = line.split_whitespace();
        let iface = cols.next()?;
        let destination = cols.next()?;
        let _gateway = cols.next()?;
        let flags = u32::from_str_radix(cols.next()?, 16).ok()?;
        (destination == "00000000" && flags & RTF_UP != 0).then_some(iface)
    })
}

/// Link kind of `iface` from whether it has a `wireless/` directory and
/// its `ARPHRD_*` type code (`linux/if_arp.h`).
#[cfg(any(target_os = "linux", test))]
fn classify_link(iface: &str, wireless: bool, arphrd: Option<u32>) -> LinkKind {
    const ETHER: u32 = 1;
    const TUNNEL: u32 = 768;
    const IEEE80211: u32 = 801;
    const IEEE80211_RADIOTAP: u32 = 803;

    if wireless {
        return LinkKind::Wifi;
    }
    let named = |prefixes: &[&str]| prefixes.iter().any(|p| iface.starts_with(p));
    match arphrd {
        // Mobile-broadband modems often register as plain Ethernet, so
        // their names win over the type code.
        _ if named(&["ppp", "wwan", "rmnet"]) => LinkKind::Cellular,
        _ if named(&["tun", "tap", "wg"]) => LinkKind::Vpn,
        Some(ETHER) => LinkKind::Ethernet,
        Some(IEEE80211..=IEEE80211_RADIOTAP) => LinkKind::Wifi,
        Some(TUNNEL) => LinkKind::Vpn,
        _ => LinkKind::Other,
    }
}

/// One adapter as `GetAdaptersAddresses` reports it.
#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Clone, Copy)]
struct Adapter {
    /// IANA `ifType`.
    if_type: u32,
    up: bool,
    has_gateway: bool,
}

/// Best link among `adapters`, `None` when none can reach beyond the
/// host. Loopback and tunnel pseudo-interfaces (Teredo, ISATAP) are
/// always up and never carry the default route themselves, so they are
/// ignored; so is anything without a gateway (host-only virtual NICs).
#[cfg(any(target_os = "windows", test))]
fn best_link(adapters: impl IntoIterator<Item = Adapter>) -> Option<LinkKind> {
    // IANA ifType numbers.
    const ETHERNET_CSMACD: u32 = 6;
    const PPP: u32 = 23;
    const SOFTWARE_LOOPBACK: u32 = 24;
    const IEEE80211: u32 = 71;
    const TUNNEL: u32 = 131;
    const WWANPP: u32 = 243;
    const WWANPP2: u32 = 244;

    let rank = |kind: LinkKind| match kind {
        LinkKind::Ethernet => 5,
        LinkKind::Wifi => 4,
        LinkKind::Cellular => 3,
        LinkKind::Vpn => 2,
        LinkKind::Bluetooth | LinkKind::Other => 1,
        LinkKind::None => 0,
    };
    adapters
        .into_iter()
        .filter(|a| a.up && a.has_gateway)
        .filter_map(|a| match a.if_type {
            SOFTWARE_LOOPBACK | TUNNEL => None,
            ETHERNET_CSMACD => Some(LinkKind::Ethernet),
            IEEE80211 => Some(LinkKind::Wifi),
            WWANPP | WWANPP2 => Some(LinkKind::Cellular),
            PPP => Some(LinkKind::Vpn),
            _ => Some(LinkKind::Other),
        })
        .max_by_key(|&kind| rank(kind))
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;

    use super::{NetworkState, classify_link, default_route_interface, snapshot};

    #[expect(
        clippy::unnecessary_wraps,
        reason = "shares the `Option` signature of the other platforms' samplers"
    )]
    pub(super) fn sample() -> Option<NetworkState> {
        let table = fs::read_to_string("/proc/net/route").unwrap_or_default();
        let link = default_route_interface(&table).map(|iface| {
            let wireless = fs::metadata(format!("/sys/class/net/{iface}/wireless")).is_ok();
            let arphrd = fs::read_to_string(format!("/sys/class/net/{iface}/type"))
                .ok()
                .and_then(|raw| raw.trim().parse().ok());
            classify_link(iface, wireless, arphrd)
        });
        Some(snapshot(link))
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows {
    use std::mem::size_of;
    use std::ptr;

    use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
        GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows_sys::Win32::Networking::WinSock::AF_UNSPEC;

    use super::{Adapter, NetworkState, best_link, snapshot};

    /// Retries when adapters appear between the sizing call and the
    /// fill call.
    const ATTEMPTS: usize = 3;

    pub(super) fn sample() -> Option<NetworkState> {
        let flags = GAA_FLAG_INCLUDE_GATEWAYS
            | GAA_FLAG_SKIP_ANYCAST
            | GAA_FLAG_SKIP_MULTICAST
            | GAA_FLAG_SKIP_DNS_SERVER;
        // Microsoft suggests starting at 15 KiB.
        let mut size: u32 = 15 * 1024;
        for _ in 0..ATTEMPTS {
            // `u64` storage keeps the `IP_ADAPTER_ADDRESSES_LH` records
            // 8-byte aligned; a zeroed `Vec` makes every byte initialised.
            let mut buffer = vec![0_u64; (size as usize).div_ceil(size_of::<u64>())];
            // SAFETY: `buffer` holds at least `size` writable bytes; the
            // call writes at most `size` bytes and updates `size` to the
            // required length on `ERROR_BUFFER_OVERFLOW`.
            let rc = unsafe {
                GetAdaptersAddresses(
                    u32::from(AF_UNSPEC),
                    flags,
                    ptr::null(),
                    buffer.as_mut_ptr().cast(),
                    &raw mut size,
                )
            };
            match rc {
                NO_ERROR => return Some(snapshot(best_link(adapters(&buffer)))),
                ERROR_BUFFER_OVERFLOW => {}
                _ => return None,
            }
        }
        None
    }

    /// Walk the adapter list `GetAdaptersAddresses` wrote into `buffer`.
    fn adapters(buffer: &[u64]) -> Vec<Adapter> {
        let mut out = Vec::new();
        let mut cur = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        while !cur.is_null() {
            // SAFETY: `cur` is the list head inside `buffer` or a `Next`
            // link the call wrote, which also points inside `buffer`;
            // `buffer` is borrowed for the whole walk.
            let adapter = unsafe { &*cur };
            out.push(Adapter {
                if_type: adapter.IfType,
                up: adapter.OperStatus == IfOperStatusUp,
                has_gateway: !adapter.FirstGatewayAddress.is_null(),
            });
            cur = adapter.Next;
        }
        out
    }
}

/// `nw_interface_type_t` of the path's preferred interface.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NwInterfaceType {
    Other,
    Wifi,
    Cellular,
    Wired,
    Loopback,
}

#[cfg(any(target_os = "macos", test))]
impl NwInterfaceType {
    const fn from_raw(raw: i32) -> Self {
        match raw {
            1 => Self::Wifi,
            2 => Self::Cellular,
            3 => Self::Wired,
            4 => Self::Loopback,
            _ => Self::Other,
        }
    }
}

/// Link kind of the path's preferred interface. Tunnels report
/// `other`, so the BSD name tells a VPN apart.
#[cfg(any(target_os = "macos", test))]
fn nw_link(kind: NwInterfaceType, name: &str) -> LinkKind {
    const TUNNELS: [&str; 5] = ["utun", "ipsec", "ppp", "tun", "tap"];
    if TUNNELS.iter().any(|prefix| name.starts_with(prefix)) {
        return LinkKind::Vpn;
    }
    match kind {
        NwInterfaceType::Wifi => LinkKind::Wifi,
        NwInterfaceType::Cellular => LinkKind::Cellular,
        NwInterfaceType::Wired => LinkKind::Ethernet,
        NwInterfaceType::Other | NwInterfaceType::Loopback => LinkKind::Other,
    }
}

/// Network.framework path monitor, the same source as the iOS
/// observer. The monitor runs for the rest of the process once the
/// first `tick` starts it; `tick` reads the path it last reported.
#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{CStr, c_char, c_int, c_void};
    use std::sync::{Condvar, Mutex, Once};
    use std::time::Duration;

    use block2::{Block, RcBlock, StackBlock};
    use istmo_core::sync::lock;

    use super::{LinkKind, NetworkState, NwInterfaceType, nw_link};

    /// How long the first `tick` waits for the monitor's first report.
    const FIRST_PATH_WAIT: Duration = Duration::from_millis(500);
    /// `nw_path_status_satisfied`.
    const STATUS_SATISFIED: c_int = 1;

    #[link(name = "Network", kind = "framework")]
    unsafe extern "C" {
        fn nw_path_monitor_create() -> *mut c_void;
        fn nw_path_monitor_set_queue(monitor: *mut c_void, queue: *mut c_void);
        fn nw_path_monitor_set_update_handler(
            monitor: *mut c_void,
            handler: &Block<dyn Fn(*mut c_void)>,
        );
        fn nw_path_monitor_start(monitor: *mut c_void);
        fn nw_path_get_status(path: *mut c_void) -> c_int;
        fn nw_path_is_expensive(path: *mut c_void) -> bool;
        fn nw_path_is_constrained(path: *mut c_void) -> bool;
        fn nw_path_enumerate_interfaces(
            path: *mut c_void,
            // The block returns a C `bool`; `u8` has its ABI (objc2 encodes no
            // Rust `bool`).
            enumerator: &Block<dyn Fn(*mut c_void) -> u8 + '_>,
        );
        fn nw_interface_get_type(interface: *mut c_void) -> c_int;
        fn nw_interface_get_name(interface: *mut c_void) -> *const c_char;
    }

    // libdispatch is part of libSystem.
    unsafe extern "C" {
        fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut c_void;
    }

    static LATEST: Mutex<Option<NetworkState>> = Mutex::new(None);
    static REPORTED: Condvar = Condvar::new();
    static START: Once = Once::new();

    pub(super) fn sample() -> Option<NetworkState> {
        START.call_once(start_monitor);
        // Only the first call waits: later ones find a report.
        REPORTED
            .wait_timeout_while(lock(&LATEST), FIRST_PATH_WAIT, |latest| latest.is_none())
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
            .clone()
    }

    fn start_monitor() {
        let handler = RcBlock::new(|path: *mut c_void| {
            // SAFETY: Network.framework hands the handler a live path,
            // valid for the duration of the call.
            let state = unsafe { read_path(path) };
            *lock(&LATEST) = Some(state);
            REPORTED.notify_all();
        });
        // SAFETY: plain Network.framework / libdispatch calls. A NULL
        // attribute makes the queue serial, so reports arrive in order.
        // The monitor and queue are never released: they live as long
        // as the process, like the iOS observer.
        unsafe {
            let monitor = nw_path_monitor_create();
            if monitor.is_null() {
                tracing::warn!(target: "istmo.network_state", "nw_path_monitor_create failed");
                return;
            }
            let queue = dispatch_queue_create(c"istmo.network_state".as_ptr(), std::ptr::null());
            nw_path_monitor_set_queue(monitor, queue);
            // The monitor copies the block.
            nw_path_monitor_set_update_handler(monitor, &handler);
            nw_path_monitor_start(monitor);
        }
    }

    /// # Safety
    ///
    /// `path` is a live `nw_path_t`.
    unsafe fn read_path(path: *mut c_void) -> NetworkState {
        // SAFETY: forwarded from the caller.
        let online = unsafe { nw_path_get_status(path) } == STATUS_SATISFIED;
        if !online {
            return NetworkState {
                online,
                link: LinkKind::None,
                metered: false,
                expensive: false,
                constrained: false,
            };
        }
        let preferred = std::cell::Cell::new(None);
        // Interfaces come in order of preference: keep the first, then
        // stop (`0` is `false`).
        let enumerator = StackBlock::new(|interface: *mut c_void| -> u8 {
            // SAFETY: the enumerator receives live interfaces; the name
            // is a NUL terminated string owned by the interface.
            let (kind, name) = unsafe {
                let name = nw_interface_get_name(interface);
                let name = if name.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(name).to_string_lossy().into_owned()
                };
                (
                    NwInterfaceType::from_raw(nw_interface_get_type(interface)),
                    name,
                )
            };
            preferred.set(Some(nw_link(kind, &name)));
            0
        });
        // SAFETY: synchronous enumeration of the live path.
        unsafe { nw_path_enumerate_interfaces(path, &enumerator) };
        // SAFETY: forwarded from the caller.
        let (expensive, constrained) =
            unsafe { (nw_path_is_expensive(path), nw_path_is_constrained(path)) };
        NetworkState {
            online,
            link: preferred.get().unwrap_or(LinkKind::Other),
            // macOS has no separate "metered" flag; expensive paths
            // (cellular, personal hotspot) are the metered ones.
            metered: expensive,
            expensive,
            constrained,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTES: &str = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0
wlp3s0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0
wlp3s0\t0001A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0
";

    #[test]
    fn default_route_is_the_zero_destination_that_is_up() {
        assert_eq!(default_route_interface(ROUTES), Some("wlp3s0"));
    }

    #[test]
    fn route_table_without_default_route_is_offline() {
        let table = "Iface\tDestination\tGateway\tFlags\nlo\t0000007F\t00000000\t0001\n";
        assert_eq!(default_route_interface(table), None);
        assert_eq!(default_route_interface(""), None);
        let down = "Iface\tDestination\tGateway\tFlags\neth0\t00000000\t0101A8C0\t0002\n";
        assert_eq!(default_route_interface(down), None);
    }

    #[test]
    fn links_classify_by_wireless_dir_name_and_arphrd() {
        assert_eq!(classify_link("wlp3s0", true, Some(1)), LinkKind::Wifi);
        assert_eq!(
            classify_link("enp0s31f6", false, Some(1)),
            LinkKind::Ethernet
        );
        assert_eq!(classify_link("wwan0", false, Some(1)), LinkKind::Cellular);
        assert_eq!(classify_link("tun0", false, Some(65534)), LinkKind::Vpn);
        assert_eq!(classify_link("wg0", false, Some(65534)), LinkKind::Vpn);
        assert_eq!(classify_link("mon0", false, Some(803)), LinkKind::Wifi);
        assert_eq!(classify_link("x", false, None), LinkKind::Other);
    }

    const fn adapter(if_type: u32, up: bool, has_gateway: bool) -> Adapter {
        Adapter {
            if_type,
            up,
            has_gateway,
        }
    }

    #[test]
    fn loopback_and_tunnels_never_make_the_host_online() {
        let only_pseudo = [adapter(24, true, false), adapter(131, true, true)];
        assert_eq!(best_link(only_pseudo), None);
        assert!(!snapshot(best_link(only_pseudo)).online);
    }

    #[test]
    fn adapters_without_gateway_or_down_are_ignored() {
        assert_eq!(
            best_link([adapter(6, true, false), adapter(71, false, true)]),
            None
        );
    }

    #[test]
    fn best_ranked_link_wins_regardless_of_order() {
        let adapters = [
            adapter(53, true, true),
            adapter(71, true, true),
            adapter(6, true, true),
        ];
        assert_eq!(best_link(adapters), Some(LinkKind::Ethernet));
        assert_eq!(
            best_link([adapter(53, true, true), adapter(243, true, true)]),
            Some(LinkKind::Cellular)
        );
        let state = snapshot(best_link([adapter(71, true, true)]));
        assert!(state.online);
        assert_eq!(state.link, LinkKind::Wifi);
    }

    #[test]
    fn nw_tunnels_are_vpn_and_types_map_to_links() {
        assert_eq!(nw_link(NwInterfaceType::Other, "utun3"), LinkKind::Vpn);
        assert_eq!(nw_link(NwInterfaceType::Wifi, "ipsec0"), LinkKind::Vpn);
        assert_eq!(nw_link(NwInterfaceType::from_raw(1), "en0"), LinkKind::Wifi);
        assert_eq!(
            nw_link(NwInterfaceType::from_raw(2), "pdp_ip0"),
            LinkKind::Cellular
        );
        assert_eq!(
            nw_link(NwInterfaceType::from_raw(3), "en5"),
            LinkKind::Ethernet
        );
        assert_eq!(
            nw_link(NwInterfaceType::from_raw(4), "lo0"),
            LinkKind::Other
        );
        assert_eq!(
            nw_link(NwInterfaceType::from_raw(99), "bridge0"),
            LinkKind::Other
        );
    }
}
