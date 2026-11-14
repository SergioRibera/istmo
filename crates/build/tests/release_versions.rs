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

/// The facade's `pre-release-replacements` entries.
fn replacements() -> Vec<toml_edit::InlineTable> {
    let manifest = root_manifest();
    manifest["package"]["metadata"]["release"]["pre-release-replacements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_inline_table().unwrap().clone())
        .collect()
}

/// Files the facade's `pre-release-replacements` rewrite.
fn replaced_files() -> Vec<String> {
    replacements()
        .iter()
        .map(|entry| entry["file"].as_str().unwrap().to_owned())
        .collect()
}

/// Versions of istmo a doc file tells readers to use: crate
/// requirements (`istmo-core = "…"`, `istmo = { version = "…", … }`),
/// the runtime and Gradle plugin coordinates, the Swift package `from:`.
fn documented_versions(text: &str) -> Vec<String> {
    let quoted = |rest: &str| rest.split('"').next().unwrap_or_default().to_owned();
    let mut found = Vec::new();
    for line in text.lines() {
        let line = line.trim_start().trim_start_matches("# ");
        if line.starts_with("istmo")
            && let Some((name, value)) = line.split_once('=')
            && !name.trim().contains(' ')
        {
            let value = value.trim_start();
            if let Some(rest) = value.strip_prefix('"') {
                found.push(quoted(rest));
            } else if let Some(rest) = value.strip_prefix("{ version = \"") {
                found.push(quoted(rest));
            }
        }
        for needle in [
            "io.github.sergioribera:istmo-runtime:",
            "id(\"io.github.sergioribera.istmo\") version \"",
            "github.com/SergioRibera/istmo.git\", from: \"",
        ] {
            found.extend(line.split(needle).skip(1).map(quoted));
        }
    }
    // Placeholders such as `<version>` are not versions.
    found.retain(|v| v.starts_with(|c: char| c.is_ascii_digit()));
    found
}

/// Markdown under `dir`, recursively, as paths relative to the repo root.
fn markdown_under(dir: &Path, out: &mut Vec<String>) {
    for entry in fs::read_dir(repo_root().join(dir)).unwrap() {
        let entry = entry.unwrap();
        let path = dir.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            markdown_under(&path, out);
        } else if path
            .extension()
            .is_some_and(|ext| ext == "md" || ext == "mdx")
        {
            out.push(path.to_string_lossy().into_owned());
        }
    }
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

/// cargo-release skips a replacement on pre-release versions unless it
/// says otherwise, and every istmo release so far is one.
#[test]
fn every_replacement_runs_on_pre_releases() {
    for entry in replacements() {
        assert_eq!(
            entry.get("prerelease").and_then(|v| v.as_bool()),
            Some(true),
            "{} needs `prerelease = true`",
            entry["file"].as_str().unwrap_or_default()
        );
    }
}

#[test]
fn documented_versions_share_the_release_version() {
    let version = release_version();
    let replaced = replaced_files();
    let mut files = vec!["README.md".to_owned(), "runtime/README.md".to_owned()];
    for plugin in fs::read_dir(repo_root().join("plugins")).unwrap() {
        let readme = Path::new("plugins")
            .join(plugin.unwrap().file_name())
            .join("README.md");
        if repo_root().join(&readme).is_file() {
            files.push(readme.to_string_lossy().into_owned());
        }
    }
    markdown_under(Path::new("docs/src/content/docs"), &mut files);
    let mut checked = 0;
    for file in files {
        let text = fs::read_to_string(repo_root().join(&file)).unwrap();
        let declared = documented_versions(&text);
        if declared.is_empty() {
            continue;
        }
        for found in &declared {
            assert_eq!(found, &version, "{file}");
        }
        assert!(
            replaced.contains(&file),
            "{file} missing from pre-release-replacements"
        );
        checked += declared.len();
    }
    assert!(checked > 0, "no doc names an istmo version");
}
