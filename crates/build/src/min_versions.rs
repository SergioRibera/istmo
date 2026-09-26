//! Minimum platform versions declared by plugins and apps.
//!
//! Plugins declare the oldest OS release their native backends support
//! in `istmo.toml`:
//!
//! ```toml
//! [min_versions]
//! android = 24          # API level
//! ios = "15.0"
//! macos = "11.0"
//! windows = "10.0.17763"
//! ```
//!
//! Apps may declare the same table to state the floor they ship to.
//! [`check_min_versions`] compares every dependency's requirements
//! against the app's floor — taken from the app's own `[min_versions]`
//! table when present, otherwise detected from the build environment
//! (`minSdk` in `android/app/build.gradle(.kts)`,
//! `IPHONEOS_DEPLOYMENT_TARGET`, `MACOSX_DEPLOYMENT_TARGET`). A plugin
//! that needs a newer OS than the app targets fails the build with a
//! message naming the plugin, the platform and both versions.
//!
//! The Gradle plugin loader repeats the Android check against the
//! resolved `minSdk` of the variant being built, and the generated
//! xcodegen fragment repeats the Apple checks inside Xcode, so the
//! guarantee holds even when `cargo` runs with a stale environment.

use std::fmt;
use std::fs;
use std::path::Path;
use std::str::FromStr;

use bincode::{Decode, Encode};

use crate::manifest::Manifest;

/// Dotted numeric OS version (`"15"`, `"15.4"`, `"10.0.17763"`).
///
/// Missing trailing components compare as zero, so `"15"` equals
/// `"15.0.0"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Encode, Decode)]
pub struct OsVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl OsVersion {
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for OsVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.patch == 0 {
            write!(f, "{}.{}", self.major, self.minor)
        } else {
            write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
        }
    }
}

/// Error returned when a version literal is not `major[.minor[.patch]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidOsVersion(pub String);

impl fmt::Display for InvalidOsVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid OS version `{}` (expected `major[.minor[.patch]]`)",
            self.0
        )
    }
}

impl std::error::Error for InvalidOsVersion {}

impl FromStr for OsVersion {
    type Err = InvalidOsVersion;

    fn from_str(literal: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidOsVersion(literal.to_owned());
        let mut parts = literal.trim().split('.');
        let mut next = |required: bool| -> Result<u32, InvalidOsVersion> {
            match parts.next() {
                Some(part) => part.parse().map_err(|_| invalid()),
                None if required => Err(invalid()),
                None => Ok(0),
            }
        };
        let version = Self::new(next(true)?, next(false)?, next(false)?);
        if parts.next().is_some() {
            return Err(invalid());
        }
        Ok(version)
    }
}

/// Platforms a `[min_versions]` table can constrain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MinVersionPlatform {
    Android,
    Ios,
    Macos,
    Windows,
}

impl MinVersionPlatform {
    /// `istmo.toml` key naming this platform.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Android => "android",
            Self::Ios => "ios",
            Self::Macos => "macos",
            Self::Windows => "windows",
        }
    }

    /// Platform matching a Cargo `CARGO_CFG_TARGET_OS` value.
    #[must_use]
    pub fn from_target_os(target_os: &str) -> Option<Self> {
        match target_os {
            "android" => Some(Self::Android),
            "ios" => Some(Self::Ios),
            "macos" => Some(Self::Macos),
            "windows" => Some(Self::Windows),
            _ => None,
        }
    }
}

/// Parsed `[min_versions]` table. Absent keys mean "no constraint".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Encode, Decode)]
pub struct MinVersions {
    /// Android API level.
    pub android: Option<u32>,
    pub ios: Option<OsVersion>,
    pub macos: Option<OsVersion>,
    pub windows: Option<OsVersion>,
}

impl MinVersions {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.android.is_none()
            && self.ios.is_none()
            && self.macos.is_none()
            && self.windows.is_none()
    }

    /// Requirement for `platform`, with Android API levels lifted into
    /// an [`OsVersion`] (`major` = API level) so every platform compares
    /// through the same type.
    #[must_use]
    pub fn get(&self, platform: MinVersionPlatform) -> Option<OsVersion> {
        match platform {
            MinVersionPlatform::Android => self.android.map(|api| OsVersion::new(api, 0, 0)),
            MinVersionPlatform::Ios => self.ios,
            MinVersionPlatform::Macos => self.macos,
            MinVersionPlatform::Windows => self.windows,
        }
    }

    /// Combine two requirement sets, keeping the stricter bound per
    /// platform.
    #[must_use]
    pub fn max(self, other: Self) -> Self {
        Self {
            android: self.android.max(other.android),
            ios: self.ios.max(other.ios),
            macos: self.macos.max(other.macos),
            windows: self.windows.max(other.windows),
        }
    }
}

/// Where the app's floor for a platform was read from. Only used to
/// make violation messages actionable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FloorSource {
    AppManifest,
    Gradle(String),
    Env(&'static str),
}

impl fmt::Display for FloorSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AppManifest => f.write_str("the app's istmo.toml [min_versions]"),
            Self::Gradle(path) => write!(f, "minSdk in {path}"),
            Self::Env(var) => write!(f, "${var}"),
        }
    }
}

/// A plugin that requires a newer OS than the app targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinVersionViolation {
    pub plugin: String,
    pub platform: MinVersionPlatform,
    pub required: OsVersion,
    pub app_floor: OsVersion,
    pub source: FloorSource,
}

impl fmt::Display for MinVersionViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (required, floor) = match self.platform {
            MinVersionPlatform::Android => (
                format!("API {}", self.required.major),
                format!("API {}", self.app_floor.major),
            ),
            MinVersionPlatform::Ios | MinVersionPlatform::Macos | MinVersionPlatform::Windows => {
                (self.required.to_string(), self.app_floor.to_string())
            }
        };
        write!(
            f,
            "plugin `{}` requires {} >= {required}, but the app targets {floor} ({}); \
             raise the app's minimum {} version or drop the plugin",
            self.plugin,
            self.platform.key(),
            self.source,
            self.platform.key(),
        )
    }
}

impl std::error::Error for MinVersionViolation {}

/// A plugin's declared requirements, keyed by its primary id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRequirement<'a> {
    pub plugin: &'a str,
    pub min_versions: MinVersions,
}

/// Compare every plugin requirement against the app floor for
/// `platform`. Returns every violation, not only the first, so a
/// single build surfaces all offending plugins.
#[must_use]
pub fn check_min_versions(
    platform: MinVersionPlatform,
    app_floor: OsVersion,
    source: &FloorSource,
    requirements: &[PluginRequirement<'_>],
) -> Vec<MinVersionViolation> {
    requirements
        .iter()
        .filter_map(|req| {
            let required = req.min_versions.get(platform)?;
            (required > app_floor).then(|| MinVersionViolation {
                plugin: req.plugin.to_owned(),
                platform,
                required,
                app_floor,
                source: source.clone(),
            })
        })
        .collect()
}

/// Resolve the app's floor for `platform`: the app manifest wins, then
/// the build environment. `None` when nothing declares a floor — the
/// check is skipped rather than guessed.
#[must_use]
pub fn detect_app_floor(
    platform: MinVersionPlatform,
    app: &MinVersions,
    app_root: &Path,
) -> Option<(OsVersion, FloorSource)> {
    if let Some(version) = app.get(platform) {
        return Some((version, FloorSource::AppManifest));
    }
    match platform {
        MinVersionPlatform::Android => detect_gradle_min_sdk(&app_root.join("android/app"))
            .map(|(api, path)| (OsVersion::new(api, 0, 0), FloorSource::Gradle(path))),
        MinVersionPlatform::Ios => env_version("IPHONEOS_DEPLOYMENT_TARGET"),
        MinVersionPlatform::Macos => env_version("MACOSX_DEPLOYMENT_TARGET"),
        MinVersionPlatform::Windows => None,
    }
}

/// Build-script entry point: check every dependency manifest against
/// the app floor of the platform being compiled for, and fail the build
/// on the first batch of violations.
///
/// Called from [`emit_with`](crate::emit_with); no-op for targets that
/// no `[min_versions]` key covers (Linux, wasm, …).
///
/// # Panics
///
/// When at least one plugin requires a newer OS than the app targets —
/// panicking is how a build script fails the build.
pub fn enforce_min_versions(dep_manifests: &[Manifest], app: Option<&Manifest>, app_root: &Path) {
    let Some(platform) = std::env::var("CARGO_CFG_TARGET_OS")
        .ok()
        .as_deref()
        .and_then(MinVersionPlatform::from_target_os)
    else {
        return;
    };
    match platform {
        MinVersionPlatform::Ios => {
            println!("cargo:rerun-if-env-changed=IPHONEOS_DEPLOYMENT_TARGET");
        }
        MinVersionPlatform::Macos => {
            println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
        }
        MinVersionPlatform::Android | MinVersionPlatform::Windows => {}
    }
    let app_versions = app.map(|m| m.min_versions).unwrap_or_default();
    let Some((floor, source)) = detect_app_floor(platform, &app_versions, app_root) else {
        return;
    };
    let requirements: Vec<PluginRequirement<'_>> = dep_manifests
        .iter()
        .filter(|m| !m.min_versions.is_empty() && !m.plugins.is_empty())
        .map(|m| PluginRequirement {
            plugin: m.primary_id(),
            min_versions: m.min_versions,
        })
        .collect();
    let violations = check_min_versions(platform, floor, &source, &requirements);
    if violations.is_empty() {
        return;
    }
    let report = violations
        .iter()
        .map(|v| format!("  - {v}"))
        .collect::<Vec<_>>()
        .join("\n");
    panic!("istmo-build: minimum platform version check failed:\n{report}");
}

fn env_version(var: &'static str) -> Option<(OsVersion, FloorSource)> {
    let value = std::env::var(var).ok()?;
    let version = value.parse().ok()?;
    Some((version, FloorSource::Env(var)))
}

/// Read `minSdk = N` / `minSdkVersion N` / `minSdkVersion(N)` from the
/// Android app module's Gradle build file.
#[must_use]
pub fn detect_gradle_min_sdk(app_module: &Path) -> Option<(u32, String)> {
    ["build.gradle.kts", "build.gradle"]
        .into_iter()
        .map(|name| app_module.join(name))
        .find_map(|path| {
            let text = fs::read_to_string(&path).ok()?;
            let api = parse_min_sdk(&text)?;
            Some((api, path.display().to_string()))
        })
}

fn parse_min_sdk(source: &str) -> Option<u32> {
    source.lines().find_map(|line| {
        let trimmed = line.trim_start();
        let rest = trimmed
            .strip_prefix("minSdkVersion")
            .or_else(|| trimmed.strip_prefix("minSdk"))?;
        let digits: String = rest
            .trim_start_matches(|c: char| c.is_whitespace() || c == '=' || c == '(')
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partial_versions() {
        assert_eq!("15".parse(), Ok(OsVersion::new(15, 0, 0)));
        assert_eq!("15.4".parse(), Ok(OsVersion::new(15, 4, 0)));
        assert_eq!("10.0.17763".parse(), Ok(OsVersion::new(10, 0, 17763)));
    }

    #[test]
    fn rejects_malformed_versions() {
        assert!("".parse::<OsVersion>().is_err());
        assert!("15.x".parse::<OsVersion>().is_err());
        assert!("1.2.3.4".parse::<OsVersion>().is_err());
    }

    #[test]
    fn orders_numerically_not_lexically() {
        let a: OsVersion = "10.0.9".parse().unwrap();
        let b: OsVersion = "10.0.17763".parse().unwrap();
        assert!(a < b);
    }

    #[test]
    fn max_keeps_stricter_bound() {
        let a = MinVersions {
            android: Some(21),
            ios: Some(OsVersion::new(16, 0, 0)),
            ..MinVersions::default()
        };
        let b = MinVersions {
            android: Some(24),
            macos: Some(OsVersion::new(11, 0, 0)),
            ..MinVersions::default()
        };
        let merged = a.max(b);
        assert_eq!(merged.android, Some(24));
        assert_eq!(merged.ios, Some(OsVersion::new(16, 0, 0)));
        assert_eq!(merged.macos, Some(OsVersion::new(11, 0, 0)));
        assert_eq!(merged.windows, None);
    }

    #[test]
    fn reports_every_violation() {
        let reqs = [
            PluginRequirement {
                plugin: "istmo.share",
                min_versions: MinVersions {
                    android: Some(29),
                    ..MinVersions::default()
                },
            },
            PluginRequirement {
                plugin: "istmo.ok",
                min_versions: MinVersions {
                    android: Some(21),
                    ..MinVersions::default()
                },
            },
            PluginRequirement {
                plugin: "istmo.other",
                min_versions: MinVersions {
                    android: Some(33),
                    ..MinVersions::default()
                },
            },
        ];
        let violations = check_min_versions(
            MinVersionPlatform::Android,
            OsVersion::new(26, 0, 0),
            &FloorSource::AppManifest,
            &reqs,
        );
        let plugins: Vec<_> = violations.iter().map(|v| v.plugin.as_str()).collect();
        assert_eq!(plugins, ["istmo.share", "istmo.other"]);
        let message = violations[0].to_string();
        assert!(message.contains("API 29"), "{message}");
        assert!(message.contains("API 26"), "{message}");
    }

    #[test]
    fn parses_gradle_min_sdk_forms() {
        assert_eq!(parse_min_sdk("    minSdk = 26\n"), Some(26));
        assert_eq!(parse_min_sdk("minSdkVersion 21"), Some(21));
        assert_eq!(parse_min_sdk("  minSdkVersion(23)"), Some(23));
        assert_eq!(parse_min_sdk("compileSdk = 34"), None);
    }
}
