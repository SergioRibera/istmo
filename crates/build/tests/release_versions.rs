//! The Android runtime AAR and the Gradle plugin are published with the
//! crates' release version; cargo-release rewrites them through the
//! facade's `pre-release-replacements`. These checks keep the checked-in
//! values and that list from drifting.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn root_manifest() -> toml_edit::DocumentMut {
    fs::read_to_string(repo_root().join("Cargo.toml"))
        .unwrap()
        .parse()
        .unwrap()
}

fn release_version() -> String {
    root_manifest()["package"]["version"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Files the facade's `pre-release-replacements` rewrite.
fn replaced_files() -> Vec<String> {
    let manifest = root_manifest();
    manifest["package"]["metadata"]["release"]["pre-release-replacements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            entry.as_inline_table().unwrap()["file"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

fn property(file: &str, key: &str) -> String {
    let text = fs::read_to_string(repo_root().join(file)).unwrap();
    text.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .unwrap_or_else(|| panic!("{file} has no `{key}`"))
        .trim()
        .to_owned()
}

#[test]
fn gradle_artifacts_share_the_release_version() {
    let version = release_version();
    let replaced = replaced_files();
    for (file, key) in [
        ("runtime/android/gradle.properties", "runtimeVersion"),
        ("runtime/gradle-plugin/gradle.properties", "pluginVersion"),
    ] {
        assert_eq!(property(file, key), version, "{file} `{key}`");
        assert!(
            replaced.iter().any(|f| f == file),
            "{file} missing from pre-release-replacements"
        );
    }
}

#[test]
fn example_runtime_dependencies_share_the_release_version() {
    let version = release_version();
    let replaced = replaced_files();
    let needle = "io.github.sergioribera:istmo-runtime:";
    let mut checked = 0;
    for example in fs::read_dir(repo_root().join("examples")).unwrap() {
        let name = example.unwrap().file_name();
        let file = format!(
            "examples/{}/android/app/build.gradle.kts",
            name.to_string_lossy()
        );
        let Ok(text) = fs::read_to_string(repo_root().join(&file)) else {
            continue;
        };
        for declared in text.split(needle).skip(1) {
            let declared = declared.split('"').next().unwrap();
            assert_eq!(declared, version, "{file}");
            assert!(
                replaced.contains(&file),
                "{file} missing from pre-release-replacements"
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "no example declares istmo-runtime");
}
