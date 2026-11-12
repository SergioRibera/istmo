//! Native project layout detection: the Android namespace and the iOS
//! app directory.

use std::fs;
use std::path::{Path, PathBuf};

#[must_use]
pub fn detect_android_package(android_root: &Path) -> Option<String> {
    ["build.gradle.kts", "build.gradle"]
        .into_iter()
        .find_map(|name| {
            let text = fs::read_to_string(android_root.join("app").join(name)).ok()?;
            extract_namespace(&text)
        })
        .or_else(|| {
            let text =
                fs::read_to_string(android_root.join("app/src/main/AndroidManifest.xml")).ok()?;
            extract_manifest_package(&text)
        })
}

fn extract_namespace(source: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("namespace") {
            let rest = rest.trim_start();
            let rest = rest.strip_prefix('=')?.trim_start();
            let quoted = rest.strip_prefix('"').or_else(|| rest.strip_prefix('\''))?;
            let end = quoted.find(['"', '\''])?;
            return Some(quoted[..end].to_owned());
        }
    }
    None
}

fn extract_manifest_package(xml: &str) -> Option<String> {
    let idx = xml.find("package=")?;
    let after = &xml[idx + "package=".len()..];
    let quoted = after
        .strip_prefix('"')
        .or_else(|| after.strip_prefix('\''))?;
    let end = quoted.find(['"', '\''])?;
    Some(quoted[..end].to_owned())
}

#[must_use]
pub fn detect_ios_app_dir(ios_root: &Path) -> Option<String> {
    let entries = fs::read_dir(ios_root).ok()?;
    let mut candidates: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| !n.starts_with('.'))
                && !p
                    .extension()
                    .is_some_and(|e| e == "xcodeproj" || e == "xcworkspace")
        })
        .collect();

    if candidates.len() > 1 {
        candidates.retain(|dir| {
            let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
            ios_root.join(format!("{name}.xcodeproj")).exists()
                || ios_root.join(format!("{name}.xcworkspace")).exists()
        });
    }

    let picked = if candidates.len() == 1 {
        candidates.into_iter().next()
    } else {
        None
    }?;
    picked
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_namespace_from_kts() {
        let src = "android {\n    namespace = \"dev.istmo.demo\"\n}\n";
        assert_eq!(extract_namespace(src).as_deref(), Some("dev.istmo.demo"));
    }

    #[test]
    fn extracts_namespace_from_groovy_single_quote() {
        let src = "android {\n    namespace 'dev.istmo.demo'\n}\n";
        assert_eq!(extract_namespace(src).as_deref(), None);
        let src = "android {\n    namespace = 'dev.istmo.demo'\n}\n";
        assert_eq!(extract_namespace(src).as_deref(), Some("dev.istmo.demo"));
    }

    #[test]
    fn extracts_namespace_with_extra_spaces() {
        let src = "android {\n   namespace   =    \"a.b.c\"\n}\n";
        assert_eq!(extract_namespace(src).as_deref(), Some("a.b.c"));
    }

    #[test]
    fn extracts_manifest_package() {
        let xml = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="dev.istmo.legacy">"#;
        assert_eq!(
            extract_manifest_package(xml).as_deref(),
            Some("dev.istmo.legacy")
        );
    }
}
