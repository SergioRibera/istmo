//! Desktop deployment-artefact codegen: systemd, launchd, Windows Service,
//! XDG `.desktop` files.
//!
//! Backend on desktop is 100% Rust — nothing crosses a language boundary —
//! so the [`crate::Contract`] / Kotlin / Swift generators do not apply. What
//! desktop plugins *do* need is a per-platform packaging artefact that tells
//! the OS init system how to launch, restart and stop the bin.
//!
//! This module is intentionally OS-neutral at the data model: one
//! [`DesktopServiceContract`] value renders into each of systemd's
//! `.service`, launchd's `.plist`, and a Windows `sc.exe` install script.
//! Platform-specific tweaks live in optional fields that the other renderers
//! ignore.
//!
//! GUI apps (rather than background services) go through
//! [`DesktopAppContract`] + [`generate_desktop_entry`] for the Linux XDG
//! `.desktop` file. macOS `.app` bundles and Windows shortcuts are out of
//! scope — those are handled by the packager (`cargo-bundle`, `WiX`) rather
//! than per-plugin codegen.

use std::fmt::Write as _;

/// Restart behaviour after the process exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    /// Never restart automatically.
    Never,
    /// Restart on abnormal exit (non-zero status, signal). systemd
    /// `Restart=on-failure`, launchd bare `KeepAlive`, Windows SC
    /// `FailureActions restart/…`.
    OnFailure,
    /// Restart unconditionally. systemd `Restart=always`, launchd
    /// `KeepAlive=true`, Windows SC `FailureActions restart/…` on every code.
    Always,
}

/// Whether the OS starts the unit at boot / login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartType {
    /// Enabled at boot / login. systemd `WantedBy=multi-user.target`,
    /// launchd `RunAtLoad=true`, Windows `start=auto`.
    Auto,
    /// User must start it explicitly. Windows `start=demand`; systemd
    /// unit installed but not enabled; launchd `RunAtLoad=false`.
    Manual,
    /// Installed but disabled. Windows `start=disabled`.
    Disabled,
}

/// Which init-system scope the unit installs into. Affects filenames and
/// systemd `WantedBy=` / launchd `~/Library/LaunchAgents` vs
/// `/Library/LaunchDaemons`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceScope {
    /// Runs as the invoking user. systemd `--user`, launchd `LaunchAgent`.
    User,
    /// System-wide, runs as `user` (or root by default). systemd system
    /// unit, launchd `LaunchDaemon`, Windows Service (`LocalSystem`).
    System,
}

/// OS-neutral service description. Feed the same value into
/// [`generate_systemd_unit`], [`generate_launchd_plist`] and
/// [`generate_windows_service`]; each renderer picks the fields it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopServiceContract {
    /// Short identifier used as filename base (e.g. `myapp-sync` →
    /// `myapp-sync.service`, `com.example.myapp.sync.plist`).
    pub name: String,
    /// Reverse-DNS style label used for launchd `Label` and Windows service
    /// name. Defaults to `name` on [`Self::new`].
    pub label: String,
    /// Human-readable description. systemd `Description=`, Windows
    /// `DisplayName`, launchd `Comment` (informational only).
    pub description: String,
    /// Absolute path to the bin.
    pub exec_path: String,
    /// Args passed to the bin.
    pub args: Vec<String>,
    /// Working directory. None → inherit / OS default.
    pub working_directory: Option<String>,
    /// Env vars set before launch.
    pub env: Vec<(String, String)>,
    /// Restart behaviour.
    pub restart: RestartPolicy,
    /// Start behaviour at boot / login.
    pub start_type: StartType,
    /// Install scope.
    pub scope: ServiceScope,
    /// User to run as (systemd `User=`, launchd `UserName`). Ignored for
    /// [`ServiceScope::User`]. `None` on system scope → root.
    pub user: Option<String>,
    /// Group to run as (systemd `Group=`, launchd `GroupName`).
    pub group: Option<String>,
    /// systemd `After=` units. Ignored by other renderers.
    pub after: Vec<String>,
    /// Windows service display name. `None` uses [`Self::description`].
    pub windows_display_name: Option<String>,
    /// Windows service dependencies (`depend=` on `sc create`).
    pub windows_dependencies: Vec<String>,
    /// launchd `KeepAlive` override — `Some(true/false)` overrides the
    /// value derived from [`Self::restart`]; `None` uses the default
    /// mapping.
    pub launchd_keep_alive: Option<bool>,
    /// Seconds to wait for graceful stop before force-kill. systemd
    /// `TimeoutStopSec=`, Windows `sc failure` reset timeout.
    pub stop_timeout_seconds: u32,
}

impl DesktopServiceContract {
    /// Convenience constructor with sane defaults: `RestartPolicy::OnFailure`,
    /// `StartType::Manual`, `ServiceScope::User`, empty args / env / after,
    /// 30-second stop timeout.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        exec_path: impl Into<String>,
    ) -> Self {
        let name = name.into();
        Self {
            label: name.clone(),
            name,
            description: description.into(),
            exec_path: exec_path.into(),
            args: Vec::new(),
            working_directory: None,
            env: Vec::new(),
            restart: RestartPolicy::OnFailure,
            start_type: StartType::Manual,
            scope: ServiceScope::User,
            user: None,
            group: None,
            after: Vec::new(),
            windows_display_name: None,
            windows_dependencies: Vec::new(),
            launchd_keep_alive: None,
            stop_timeout_seconds: 30,
        }
    }
}

/// Bundle returned by [`generate_windows_service`]. Windows install /
/// uninstall are non-idempotent shell steps rather than a single manifest,
/// hence the split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsServiceArtifacts {
    /// PowerShell script that creates the service via `sc.exe create` +
    /// configures failure actions and start type.
    pub install_script: String,
    /// PowerShell script that stops and deletes the service.
    pub uninstall_script: String,
}

/// Render a systemd `.service` unit for `contract`.
///
/// The caller decides whether the file lands in `/etc/systemd/system/`
/// (system scope) or `~/.config/systemd/user/` (user scope) — the unit
/// itself is the same text; only [`DesktopServiceContract::scope`] changes
/// the `WantedBy=` target.
#[must_use]
pub fn generate_systemd_unit(contract: &DesktopServiceContract) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# GENERATED by istmo-build - DO NOT EDIT.");
    let _ = writeln!(out, "# service name: {}", contract.name);
    let _ = writeln!(out);
    let _ = writeln!(out, "[Unit]");
    let _ = writeln!(out, "Description={}", contract.description);
    for after in &contract.after {
        let _ = writeln!(out, "After={after}");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "[Service]");
    let _ = writeln!(out, "Type=simple");
    let exec = quote_systemd_exec(&contract.exec_path, &contract.args);
    let _ = writeln!(out, "ExecStart={exec}");
    if let Some(wd) = &contract.working_directory {
        let _ = writeln!(out, "WorkingDirectory={wd}");
    }
    for (k, v) in &contract.env {
        let _ = writeln!(
            out,
            "Environment=\"{k}={v}\"",
            k = k,
            v = escape_double_quote(v)
        );
    }
    if contract.scope == ServiceScope::System {
        if let Some(user) = &contract.user {
            let _ = writeln!(out, "User={user}");
        }
        if let Some(group) = &contract.group {
            let _ = writeln!(out, "Group={group}");
        }
    }
    let _ = writeln!(
        out,
        "Restart={}",
        match contract.restart {
            RestartPolicy::Never => "no",
            RestartPolicy::OnFailure => "on-failure",
            RestartPolicy::Always => "always",
        }
    );
    let _ = writeln!(out, "TimeoutStopSec={}", contract.stop_timeout_seconds);
    let _ = writeln!(out);
    if matches!(contract.start_type, StartType::Auto) {
        let _ = writeln!(out, "[Install]");
        let target = match contract.scope {
            ServiceScope::User => "default.target",
            ServiceScope::System => "multi-user.target",
        };
        let _ = writeln!(out, "WantedBy={target}");
    }
    out
}

/// Render a launchd `.plist` for `contract`. Suitable for
/// `~/Library/LaunchAgents/<label>.plist` (user scope) or
/// `/Library/LaunchDaemons/<label>.plist` (system scope).
#[must_use]
pub fn generate_launchd_plist(contract: &DesktopServiceContract) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    let _ = writeln!(
        out,
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">",
    );
    let _ = writeln!(out, "<!-- GENERATED by istmo-build - DO NOT EDIT. -->");
    let _ = writeln!(out, "<plist version=\"1.0\">");
    let _ = writeln!(out, "<dict>");
    let _ = writeln!(out, "    <key>Label</key>");
    let _ = writeln!(out, "    <string>{}</string>", xml_escape(&contract.label));
    let _ = writeln!(out, "    <key>ProgramArguments</key>");
    let _ = writeln!(out, "    <array>");
    let _ = writeln!(
        out,
        "        <string>{}</string>",
        xml_escape(&contract.exec_path)
    );
    for arg in &contract.args {
        let _ = writeln!(out, "        <string>{}</string>", xml_escape(arg));
    }
    let _ = writeln!(out, "    </array>");
    if let Some(wd) = &contract.working_directory {
        let _ = writeln!(out, "    <key>WorkingDirectory</key>");
        let _ = writeln!(out, "    <string>{}</string>", xml_escape(wd));
    }
    if !contract.env.is_empty() {
        let _ = writeln!(out, "    <key>EnvironmentVariables</key>");
        let _ = writeln!(out, "    <dict>");
        for (k, v) in &contract.env {
            let _ = writeln!(out, "        <key>{}</key>", xml_escape(k));
            let _ = writeln!(out, "        <string>{}</string>", xml_escape(v));
        }
        let _ = writeln!(out, "    </dict>");
    }
    if contract.scope == ServiceScope::System {
        if let Some(user) = &contract.user {
            let _ = writeln!(out, "    <key>UserName</key>");
            let _ = writeln!(out, "    <string>{}</string>", xml_escape(user));
        }
        if let Some(group) = &contract.group {
            let _ = writeln!(out, "    <key>GroupName</key>");
            let _ = writeln!(out, "    <string>{}</string>", xml_escape(group));
        }
    }
    let run_at_load = matches!(contract.start_type, StartType::Auto);
    let _ = writeln!(out, "    <key>RunAtLoad</key>");
    let _ = writeln!(out, "    <{}/>", if run_at_load { "true" } else { "false" });
    let keep_alive = contract
        .launchd_keep_alive
        .unwrap_or(match contract.restart {
            RestartPolicy::Always => true,
            RestartPolicy::Never | RestartPolicy::OnFailure => false,
        });
    if keep_alive || matches!(contract.restart, RestartPolicy::OnFailure) {
        let _ = writeln!(out, "    <key>KeepAlive</key>");
        if matches!(contract.restart, RestartPolicy::OnFailure) {
            // Restart only on non-zero exit.
            let _ = writeln!(out, "    <dict>");
            let _ = writeln!(out, "        <key>SuccessfulExit</key>");
            let _ = writeln!(out, "        <false/>");
            let _ = writeln!(out, "    </dict>");
        } else {
            let _ = writeln!(out, "    <true/>");
        }
    }
    let _ = writeln!(out, "    <key>ExitTimeOut</key>");
    let _ = writeln!(
        out,
        "    <integer>{}</integer>",
        contract.stop_timeout_seconds
    );
    let _ = writeln!(out, "</dict>");
    let _ = writeln!(out, "</plist>");
    out
}

/// Render PowerShell install / uninstall scripts for a Windows service.
///
/// The install script uses `sc.exe create` + `sc.exe failure` and, when
/// [`DesktopServiceContract::description`] is set, `sc.exe description`.
/// Requires an elevated shell. The uninstall script stops then deletes the
/// service (idempotent-ish — deletion of an unknown service errors).
#[must_use]
pub fn generate_windows_service(contract: &DesktopServiceContract) -> WindowsServiceArtifacts {
    WindowsServiceArtifacts {
        install_script: render_windows_install(contract),
        uninstall_script: render_windows_uninstall(contract),
    }
}

fn render_windows_install(contract: &DesktopServiceContract) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# GENERATED by istmo-build - DO NOT EDIT.");
    let _ = writeln!(out, "# service name: {}", contract.name);
    let _ = writeln!(out, "# Requires: elevated PowerShell.");
    let _ = writeln!(out);
    let bin_path = build_windows_bin_path(&contract.exec_path, &contract.args);
    let display_name = contract
        .windows_display_name
        .as_deref()
        .unwrap_or(&contract.description);
    let start = match contract.start_type {
        StartType::Auto => "auto",
        StartType::Manual => "demand",
        StartType::Disabled => "disabled",
    };
    let mut create_args: Vec<String> = vec![
        format!("binPath= {}", quote_ps(&bin_path)),
        format!("start= {start}"),
        format!("DisplayName= {}", quote_ps(display_name)),
    ];
    if !contract.windows_dependencies.is_empty() {
        create_args.push(format!(
            "depend= {}",
            quote_ps(&contract.windows_dependencies.join("/")),
        ));
    }
    if contract.scope == ServiceScope::System {
        if let Some(user) = &contract.user {
            create_args.push(format!("obj= {}", quote_ps(user)));
        }
    }
    let _ = writeln!(
        out,
        "sc.exe create {name} {args}",
        name = contract.label,
        args = create_args.join(" "),
    );
    if !contract.description.is_empty() {
        let _ = writeln!(
            out,
            "sc.exe description {name} {desc}",
            name = contract.label,
            desc = quote_ps(&contract.description),
        );
    }
    match contract.restart {
        RestartPolicy::Never => {}
        RestartPolicy::OnFailure | RestartPolicy::Always => {
            let reset = contract.stop_timeout_seconds * 2;
            let _ = writeln!(
                out,
                "sc.exe failure {name} reset= {reset} actions= restart/5000/restart/10000/restart/30000",
                name = contract.label,
            );
        }
    }
    if !contract.env.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "# Environment variables — sc.exe cannot set them; write to the registry instead.",
        );
        let key = format!(
            "HKLM:\\SYSTEM\\CurrentControlSet\\Services\\{}\\Environment",
            contract.label,
        );
        let _ = writeln!(out, "New-Item -Path {} -Force | Out-Null", quote_ps(&key));
        for (k, v) in &contract.env {
            let _ = writeln!(
                out,
                "New-ItemProperty -Path {key} -Name {name} -Value {val} -PropertyType String -Force | Out-Null",
                key = quote_ps(&key),
                name = quote_ps(k),
                val = quote_ps(v),
            );
        }
    }
    out
}

fn render_windows_uninstall(contract: &DesktopServiceContract) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# GENERATED by istmo-build - DO NOT EDIT.");
    let _ = writeln!(out, "# service name: {}", contract.name);
    let _ = writeln!(out, "# Requires: elevated PowerShell.");
    let _ = writeln!(out);
    let _ = writeln!(out, "sc.exe stop {} | Out-Null", contract.label);
    let _ = writeln!(out, "sc.exe delete {}", contract.label);
    out
}

/// Content of an XDG `.desktop` entry for a GUI desktop app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAppContract {
    /// User-visible app name (`Name=`).
    pub name: String,
    /// Short description (`Comment=`).
    pub comment: String,
    /// Absolute path to the bin (`Exec=`). Leave `%U` / `%F` placeholders in
    /// if the app takes URL / file args.
    pub exec: String,
    /// Icon name or absolute path (`Icon=`).
    pub icon: Option<String>,
    /// XDG categories (`Categories=`). See freedesktop menu spec.
    pub categories: Vec<String>,
    /// MIME types the app handles (`MimeType=`).
    pub mime_types: Vec<String>,
    /// Show the app in the launcher menu (`NoDisplay=` inverted).
    pub visible: bool,
    /// Run in a terminal (`Terminal=`).
    pub terminal: bool,
    /// Startup notification support (`StartupNotify=`).
    pub startup_notify: bool,
    /// Keywords for launcher search (`Keywords=`).
    pub keywords: Vec<String>,
}

impl DesktopAppContract {
    /// Minimal constructor: name + exec, everything else defaulted.
    #[must_use]
    pub fn new(name: impl Into<String>, exec: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            comment: String::new(),
            exec: exec.into(),
            icon: None,
            categories: Vec::new(),
            mime_types: Vec::new(),
            visible: true,
            terminal: false,
            startup_notify: true,
            keywords: Vec::new(),
        }
    }
}

/// Render an XDG `.desktop` entry.
#[must_use]
pub fn generate_desktop_entry(contract: &DesktopAppContract) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# GENERATED by istmo-build - DO NOT EDIT.");
    let _ = writeln!(out, "[Desktop Entry]");
    let _ = writeln!(out, "Type=Application");
    let _ = writeln!(out, "Version=1.5");
    let _ = writeln!(out, "Name={}", contract.name);
    if !contract.comment.is_empty() {
        let _ = writeln!(out, "Comment={}", contract.comment);
    }
    let _ = writeln!(out, "Exec={}", contract.exec);
    if let Some(icon) = &contract.icon {
        let _ = writeln!(out, "Icon={icon}");
    }
    let _ = writeln!(out, "Terminal={}", contract.terminal);
    if !contract.visible {
        let _ = writeln!(out, "NoDisplay=true");
    }
    if contract.startup_notify {
        let _ = writeln!(out, "StartupNotify=true");
    }
    if !contract.categories.is_empty() {
        let _ = writeln!(out, "Categories={};", contract.categories.join(";"));
    }
    if !contract.mime_types.is_empty() {
        let _ = writeln!(out, "MimeType={};", contract.mime_types.join(";"));
    }
    if !contract.keywords.is_empty() {
        let _ = writeln!(out, "Keywords={};", contract.keywords.join(";"));
    }
    out
}

fn quote_systemd_exec(exec: &str, args: &[String]) -> String {
    // systemd allows unquoted paths as long as they contain no whitespace.
    // Multiple args are separated by whitespace; wrap any arg containing
    // spaces / special chars in double quotes with `\` escaping.
    let mut buf = String::new();
    buf.push_str(exec);
    for arg in args {
        buf.push(' ');
        if arg.chars().any(|c| c.is_whitespace() || c == '"') {
            buf.push('"');
            buf.push_str(&escape_double_quote(arg));
            buf.push('"');
        } else {
            buf.push_str(arg);
        }
    }
    buf
}

fn build_windows_bin_path(exec: &str, args: &[String]) -> String {
    if args.is_empty() {
        exec.to_owned()
    } else {
        format!("{} {}", exec, args.join(" "))
    }
}

fn quote_ps(value: &str) -> String {
    // PowerShell single-quoted strings: escape single quotes by doubling.
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push('\'');
        }
        out.push(ch);
    }
    out.push('\'');
    out
}

fn escape_double_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
    out
}

fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}
