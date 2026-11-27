# `istmo-autostart`

Start the app when the user logs in, and tell the app whether this
launch was such a start.

Desktop only. Android has no "start my UI at login" (the nearest thing
is `BOOT_COMPLETED` → background work, which belongs to the task
scheduler / services). iOS and the web have nothing. Those targets
register `UnavailableAutostart`, which answers `Unsupported`.
`istmo.toml` lists `targets = ["desktop"]`.

## Rules

- Never enabled without a user action: there is no `autostart = true`
  in `istmo.toml`. App stores (Mac App Store, Microsoft Store) require
  it, and an app that adds itself to login items is malware-shaped.
- Never re-enabled behind the user's back. `DisabledByUser` (the user
  turned it off in System Settings / Task Manager) is final until the
  user turns it on there, or the app calls `enable` again from a user
  action, which on those platforms opens the settings page instead.

## Backends

| Platform | Register | Status |
|---|---|---|
| macOS 13+ | `SMAppService.mainApp.register()` | `.enabled` / `.requiresApproval` / `.notRegistered` / `.notFound` |
| macOS < 13 | `~/Library/LaunchAgents/<bundle id>.istmo-autostart.plist` (`ProgramArguments`, `RunAtLoad`, `ProcessType = Interactive`) | file present and pointing at this executable |
| Windows, packaged (MSIX) | `StartupTask.GetAsync(task id).RequestEnableAsync()` (task id = `[app.autostart] windows_task_id`, default `IstmoAutostart`; the package manifest must declare the `uap5:StartupTask` extension) | `Enabled` / `Disabled` / `DisabledByUser` / `DisabledByPolicy` / `EnabledByPolicy` |
| Windows, unpackaged | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value `<bundle id>` = quoted exe + args | value present; `...\Explorer\StartupApproved\Run` byte 0 odd → `DisabledByUser` |
| Linux, Flatpak | portal `org.freedesktop.portal.Background.RequestBackground(autostart: true, commandline)` | the portal's answer; no read-back API → last answer cached in the plugin's data dir |
| Linux | `$XDG_CONFIG_HOME/autostart/<bundle id>.desktop` | file present, `Hidden`/`X-GNOME-Autostart-enabled=false` → `DisabledByUser` |

### Executable path

The path written must survive updates:

- Linux AppImage: `$APPIMAGE`, never the mount point.
- Nix: a `/nix/store/...` executable changes path on every update.
  Prefer a profile link that resolves to the same file
  (`~/.nix-profile/bin/<name>`, `/etc/profiles/per-user/$USER/bin/<name>`,
  `/run/current-system/sw/bin/<name>`); fall back to the store path.
- Everything else: `std::env::current_exe()`.

`AutostartPublisher::install` re-checks an enabled entry at every start
and rewrites it when the path or the arguments changed (Linux `.desktop`
`Exec=`, Windows `Run` value, macOS LaunchAgent). It never creates one.

## Was this a login start?

No default argument. The OS is asked first; a marker is only used where
the OS says nothing, and only when the app configures one.

| Platform | Signal |
|---|---|
| macOS (login item) | at `NSApplicationDidFinishLaunchingNotification`, `NSAppleEventManager.currentAppleEvent` is `kAEOpenApplication` with `keyAEPropData == keyAELaunchedAsLogInItem` |
| Windows MSIX | `AppInstance.GetActivatedEventArgs().Kind() == ActivationKind::StartupTask` |
| Linux with systemd (GNOME 3.34+, Plasma 5.20+) | `systemd-xdg-autostart-generator` runs the entry as `app-<id>@autostart.service`: `/proc/self/cgroup` ends in `@autostart.service` |
| Windows `Run`, Linux without systemd, Flatpak, macOS LaunchAgent | marker only |

```toml
[app.autostart]
marker = "--hidden"           # optional; no key = no marker
windows_task_id = "MyStartup" # optional, MSIX only
```

`istmo-build` emits `ISTMO_AUTOSTART_MARKER` (and
`ISTMO_AUTOSTART_TASK_ID`) for the app crate; the app hands them to the
publisher with `AutostartConfig::from_build(option_env!(…), option_env!(…))`
(same pattern as `instance_key_from_build`). `enable` appends the marker
to the registered command line; `install` detects it and removes it
from what `DocumentsPublisher` sees (`AutostartPublisher::strip_marker`
returns the filtered argv, so the app passes that on).

Published on the latest-value lane `istmo.autostart.launch`:

```rust
pub enum LaunchReason { User, Login(Detection), Unknown }
pub enum Detection { System, Marker }
```

`Unknown` = no OS signal on this platform and no marker configured.
`User` = a signal or marker exists for this platform and it was absent.
No heuristics (boot time, parent process).

## Wire types and trait

```rust
pub enum AutostartState {
    Enabled,
    Disabled,
    RequiresApproval,   // macOS: registered, waiting in System Settings
    DisabledByUser,
    DisabledByPolicy,
    Unsupported,
}

pub struct AutostartOptions {
    /// Extra arguments after the executable (the marker is added by the plugin).
    pub args: Vec<String>,
}

pub enum AutostartError {
    Unsupported,
    Denied,                 // portal / StartupTask refused
    Backend(String),
}

#[plugin(name = "istmo.autostart", remote = false)]
pub trait Autostart {
    async fn status(&self) -> Result<AutostartState, AutostartError>;
    async fn enable(&self, options: AutostartOptions) -> Result<AutostartState, AutostartError>;
    async fn disable(&self) -> Result<AutostartState, AutostartError>;
    /// macOS `SMAppService.openSystemSettingsLoginItems`, Windows
    /// `ms-settings:startupapps`, Linux: `Unsupported`.
    async fn open_settings(&self) -> Result<(), AutostartError>;
}
```

`enable` on `DisabledByUser` (Windows MSIX, Windows `StartupApproved`)
does not flip the approval bit: it opens the settings page and returns
the state unchanged. `RequiresApproval` on macOS: same.

Methods touching the registry, files, D-Bus or `SMAppService` are
`#[istmo::blocking]`.

## Crate layout

```
plugins/autostart/
  istmo.toml            id istmo.autostart, targets = ["desktop"]
  src/lib.rs            types, trait, UnavailableAutostart, LaunchReason lane
  src/desktop/mod.rs    DesktopAutostart, AutostartPublisher, exe path resolution
  src/desktop/linux.rs  XDG autostart file + Background portal (ashpd) + cgroup check
  src/desktop/windows.rs Run key (windows-registry) + StartupTask (WinRT)
  src/desktop/macos.rs  SMAppService / LaunchAgent + Apple Event check (allow unsafe_code)
  tests/                .desktop round trip in a temp XDG_CONFIG_HOME, marker stripping, path resolution
```

`istmo-build`: `[app.autostart]` parsing + the two env vars.

## Not in v1

- "Start hidden" / minimized: the app's call, driven by `LaunchReason`.
- systemd user units / launchd agents with `KeepAlive` (that is a
  service, see `#[istmo::service]`).
- Android `BOOT_COMPLETED`.

## Implementation notes (deviations from the text above)

Landed on branch `autostart`. Where the implementation differs:

- **`from_build` takes three values.** Every backend needs the app id
  (`.desktop` file name, `Run` value name, LaunchAgent label), so it is
  `AutostartConfig::from_build(bundle_id, marker, task_id)`; the
  `istmo_autostart::autostart_config!()` macro expands the three
  `option_env!`s in the app crate. Ids outside `[A-Za-z0-9._-]` are
  sanitized to `_`.
- **`AutostartPublisher::install(runtime, &config, args) -> Vec<OsString>`**
  returns the filtered argv; `strip_marker(&config, args) -> StrippedArgs
  { marker_found, args }` is the pure half. Arguments after `--` are not
  stripped.
- **`AutostartState::EnabledByPolicy`** added (MSIX reports it and
  `disable` cannot change it). `AutostartState` and `AutostartError` are
  `#[non_exhaustive]`.
- **Marker rule is stricter**: starts with `-`, a character after the
  dashes, printable ASCII without `"`, `'`, `\`, `` ` ``, `$`, `%`, at most
  64 chars (so it goes verbatim into `Exec=`, a Windows command line and
  a plist). `windows_task_id`: 1 to 64 of `[A-Za-z0-9._-]`. Same checks
  at run time in `AutostartConfig` (invalid values dropped with a warning).
- **macOS**: `SMAppService.mainApp` only when the class exists *and* the
  executable is inside `*.app/Contents/MacOS`; a bare binary on 13+ uses
  the LaunchAgent too. A user-disabled login item reads as
  `RequiresApproval` (that is what `SMAppService` reports), so there is no
  `DisabledByUser` on macOS. `open_settings` on < 13 opens the Users &
  Groups pane. The reason is published from the
  `NSApplicationDidFinishLaunchingNotification` observer (nothing until
  then); with the marker present, or without AppKit / after launch, it is
  published at once. LaunchAgent starts never carry the login-item
  property, so on that backend its absence is "unavailable", not `User`.
- **Linux**: there is no settings page, so `enable` on `DisabledByUser`
  rewrites the entry (the call is the user action). The systemd signal is
  `Absent` (→ `User`) only when the process cgroup is below
  `user@<uid>.service` (the user manager runs the session's apps);
  otherwise it is unavailable and the marker decides. Non-Linux Unix
  desktops (BSDs) are `Unsupported`.
- **Flatpak portal**: behind the default feature `linux-portal`
  (without it, Flatpak is `Unsupported`). A refused request is
  `Err(Denied)`; the answer is cached in
  `$XDG_DATA_HOME/istmo-autostart/<id>.state`. The portal command is the
  in-sandbox executable plus arguments; `install` never rewrites it.
- **Windows**: the `Run` / `StartupApproved` values go through the safe
  `windows-registry` crate; `StartupTask` / `AppInstance` / `Package`
  through `windows` 0.62 (`RoInitialize(MTA)` first, the only `unsafe`).
  Packaged = `Package::Current()` succeeds. `open_settings` runs
  `explorer.exe ms-settings:startupapps`. A `StartupTask` request that
  ends `Disabled` / `DisabledByUser` is `Err(Denied)`.
- **Nix**: a `.<name>-wrapped` store binary matches the profile link of
  its `wrapProgram` script.
- **Layout**: `src/desktop.rs` + `src/desktop/{exe,cmdline,plist,linux,
  linux/{entry,cgroup,portal},windows,macos,other}.rs` (no `mod.rs`).
  `cmdline` and `plist` compile on every desktop host so their tests run
  on Linux CI.
