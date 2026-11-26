# istmo-autostart

Start the app when the user logs in, for the
[istmo](https://github.com/SergioRibera/istmo) framework, and tell the
app whether this launch was such a start.

| Platform | Registration | Login start detected by |
| --- | --- | --- |
| macOS 13+, inside an `.app` | `SMAppService.mainApp` | `keyAELaunchedAsLogInItem` on the `oapp` Apple Event |
| macOS < 13, or a bare binary | `~/Library/LaunchAgents/<id>.istmo-autostart.plist` | marker |
| Windows, MSIX | `StartupTask` (`[app.autostart] windows_task_id`) | activation kind `StartupTask` |
| Windows, unpackaged | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` | marker |
| Linux, Flatpak | `org.freedesktop.portal.Background` (feature `linux-portal`, default) | marker |
| Linux | `$XDG_CONFIG_HOME/autostart/<id>.desktop` | `@autostart.service` cgroup (systemd sessions), else marker |

Android, iOS and the web register `UnavailableAutostart`. The plugin
lists `targets = ["desktop"]`, so it generates no Kotlin or Swift.

```rust
use istmo_autostart::{
    AutostartClient, AutostartError, AutostartHost, AutostartOptions, AutostartPublisher,
    DesktopAutostart, LaunchReasonClient,
};

let config = istmo_autostart::autostart_config!();
let args = AutostartPublisher::install(&runtime, &config, std::env::args_os().skip(1))?;
runtime.register_host(AutostartHost::new(DesktopAutostart::new(config)));

if LaunchReasonClient::from_runtime(&runtime)?.stream().recv()?.is_login() {
    // start hidden
}

// From a "Start at login" switch:
AutostartClient::from_runtime(&runtime)?
    .enable(AutostartOptions::default())
    .await
    .map_err(AutostartError::from)?;
```

```toml
# The app's istmo.toml
[app.autostart]
marker = "--autostart"        # optional
windows_task_id = "MyStartup" # optional, MSIX only
```

Autostart is never enabled without a call to `enable`, and an entry the
user turned off in the system settings stays off: `enable` opens the
settings page instead.

