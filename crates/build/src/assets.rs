//! Build-time resolution of `[[assets]]` entries.
//!
//! Each entry from an `istmo.toml` file names either a single file, a
//! directory (walked recursively), or a glob pattern rooted at the
//! crate directory. Resolution turns those entries into a flat list of
//! [`ResolvedAsset`] records that carry both the absolute source path
//! (for the copy / link step that happens on the app side) and the
//! *bundle path* — the relative key runtime code uses to look the
//! asset up via `istmo::assets::read`.

use std::io;
use std::path::{Component, Path, PathBuf};

use bincode::{Decode, Encode};
use globset::{Glob, GlobMatcher};

use crate::manifest::AssetEntry;

const GLOB_META: &[char] = &['*', '?', '[', ']'];

/// A single asset entry after resolution.
///
/// The [`bundle_path`](ResolvedAsset::bundle_path) is the addressable
/// path a caller passes to `istmo::assets::read`. It is derived from
/// the entry's declared path by stripping a leading `assets/` (or
/// `assets\`) segment when present, so authors can keep an `assets/`
/// prefix in `istmo.toml` without needing to repeat it in every
/// [`read`](../../../istmo_core/assets/fn.read.html) call.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Encode, Decode)]
pub struct ResolvedAsset {
    pub bundle_path: String,
    pub source_abs: PathBuf,
}

/// Bundle payload emitted per plugin crate and collected app-side.
///
/// Each plugin (or plugin-shaped crate) advertises a single payload;
/// the app walks every `DEP_*_ISTMO_ASSETS` env var to collect them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Encode, Decode)]
pub struct AssetsPayload {
    /// Every plugin id in the emitting crate — an asset lookup issued
    /// under any of these ids resolves to entries in [`resolved`].
    pub plugin_ids: Vec<String>,
    pub resolved: Vec<ResolvedAsset>,
}

#[derive(Debug)]
pub enum AssetResolveError {
    NotFound {
        path: PathBuf,
    },
    Io {
        path: PathBuf,
        error: io::Error,
    },
    Glob {
        pattern: String,
        error: globset::Error,
    },
    NoMatches {
        pattern: String,
    },
    OutsideRoot {
        path: PathBuf,
    },
}

impl std::fmt::Display for AssetResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { path } => write!(f, "asset not found: {}", path.display()),
            Self::Io { path, error } => write!(f, "asset io error at {}: {error}", path.display()),
            Self::Glob { pattern, error } => write!(f, "invalid asset glob `{pattern}`: {error}"),
            Self::NoMatches { pattern } => {
                write!(f, "asset glob `{pattern}` matched zero files")
            }
            Self::OutsideRoot { path } => write!(
                f,
                "asset `{}` resolves outside the crate root",
                path.display()
            ),
        }
    }
}

impl std::error::Error for AssetResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { error, .. } => Some(error),
            Self::Glob { error, .. } => Some(error),
            _ => None,
        }
    }
}

/// Walk every [`AssetEntry`], classify it and expand into
/// [`ResolvedAsset`] records.
///
/// - `entries` — from `manifest.assets`.
/// - `root`    — the crate root the entries are relative to. All
///   entries whose declared path is not already absolute are joined to
///   this directory before resolution.
pub fn resolve_assets(
    entries: &[AssetEntry],
    root: &Path,
) -> Result<Vec<ResolvedAsset>, AssetResolveError> {
    let mut out = Vec::new();
    for entry in entries {
        resolve_one(&entry.path, root, &mut out)?;
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn resolve_one(
    raw: &str,
    root: &Path,
    out: &mut Vec<ResolvedAsset>,
) -> Result<(), AssetResolveError> {
    if is_glob(raw) {
        return expand_glob(raw, root, out);
    }
    let candidate = join_root(root, raw);
    let metadata = match std::fs::metadata(&candidate) {
        Ok(m) => m,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(AssetResolveError::NotFound { path: candidate });
        }
        Err(error) => {
            return Err(AssetResolveError::Io {
                path: candidate,
                error,
            });
        }
    };
    if metadata.is_dir() {
        walk_dir(&candidate, root, out)
    } else {
        let source_abs = canonical_or_absolute(&candidate)?;
        let bundle_path = bundle_key(&source_abs, root)?;
        out.push(ResolvedAsset {
            bundle_path,
            source_abs,
        });
        Ok(())
    }
}

fn walk_dir(
    dir: &Path,
    root: &Path,
    out: &mut Vec<ResolvedAsset>,
) -> Result<(), AssetResolveError> {
    let read = std::fs::read_dir(dir).map_err(|error| AssetResolveError::Io {
        path: dir.to_path_buf(),
        error,
    })?;
    for entry in read {
        let entry = entry.map_err(|error| AssetResolveError::Io {
            path: dir.to_path_buf(),
            error,
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| AssetResolveError::Io {
                path: path.clone(),
                error,
            })?;
        if file_type.is_dir() {
            walk_dir(&path, root, out)?;
        } else if file_type.is_file() {
            let source_abs = canonical_or_absolute(&path)?;
            let bundle_path = bundle_key(&source_abs, root)?;
            out.push(ResolvedAsset {
                bundle_path,
                source_abs,
            });
        }
    }
    Ok(())
}

fn expand_glob(
    pattern: &str,
    root: &Path,
    out: &mut Vec<ResolvedAsset>,
) -> Result<(), AssetResolveError> {
    let glob = Glob::new(pattern).map_err(|error| AssetResolveError::Glob {
        pattern: pattern.to_owned(),
        error,
    })?;
    let matcher = glob.compile_matcher();
    let before = out.len();
    walk_glob(root, root, &matcher, out)?;
    if out.len() == before {
        return Err(AssetResolveError::NoMatches {
            pattern: pattern.to_owned(),
        });
    }
    Ok(())
}

fn walk_glob(
    dir: &Path,
    root: &Path,
    matcher: &GlobMatcher,
    out: &mut Vec<ResolvedAsset>,
) -> Result<(), AssetResolveError> {
    let read = std::fs::read_dir(dir).map_err(|error| AssetResolveError::Io {
        path: dir.to_path_buf(),
        error,
    })?;
    for entry in read {
        let entry = entry.map_err(|error| AssetResolveError::Io {
            path: dir.to_path_buf(),
            error,
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| AssetResolveError::Io {
                path: path.clone(),
                error,
            })?;
        if file_type.is_dir() {
            walk_glob(&path, root, matcher, out)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if matcher.is_match(rel) {
            let source_abs = canonical_or_absolute(&path)?;
            let bundle_path = bundle_key(&source_abs, root)?;
            out.push(ResolvedAsset {
                bundle_path,
                source_abs,
            });
        }
    }
    Ok(())
}

fn is_glob(path: &str) -> bool {
    path.chars().any(|c| GLOB_META.contains(&c))
}

fn join_root(root: &Path, raw: &str) -> PathBuf {
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    }
}

fn canonical_or_absolute(path: &Path) -> Result<PathBuf, AssetResolveError> {
    match std::fs::canonicalize(path) {
        Ok(p) => Ok(p),
        Err(error) => Err(AssetResolveError::Io {
            path: path.to_path_buf(),
            error,
        }),
    }
}

/// Derive the bundle-relative key for an absolute file. The rule:
///
/// 1. Strip the crate root prefix (canonicalised to match `source_abs`).
/// 2. Strip a leading `assets/` (or `assets\`) segment when present.
/// 3. Return the rest with forward slashes so the key is
///    platform-neutral.
fn bundle_key(source_abs: &Path, root: &Path) -> Result<String, AssetResolveError> {
    let root_canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let rel = source_abs
        .strip_prefix(&root_canon)
        .map_err(|_| AssetResolveError::OutsideRoot {
            path: source_abs.to_path_buf(),
        })?;
    let mut components = rel.components().peekable();
    if let Some(Component::Normal(first)) = components.peek()
        && first.to_string_lossy().eq_ignore_ascii_case("assets")
    {
        components.next();
    }
    let mut parts = Vec::new();
    for component in components {
        if let Component::Normal(part) = component {
            parts.push(part.to_string_lossy().into_owned());
        }
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("istmo-assets-test-{name}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create tmp");
        path
    }

    #[test]
    fn resolves_single_file() {
        let root = tmp_root("single");
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::write(root.join("assets/schema.sql"), b"select 1").unwrap();
        let entries = vec![AssetEntry {
            path: "assets/schema.sql".into(),
        }];
        let resolved = resolve_assets(&entries, &root).expect("resolve");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].bundle_path, "schema.sql");
    }

    #[test]
    fn resolves_recursive_dir_stripping_assets_prefix() {
        let root = tmp_root("dir");
        fs::create_dir_all(root.join("assets/img/icons")).unwrap();
        fs::write(root.join("assets/img/logo.png"), b"png").unwrap();
        fs::write(root.join("assets/img/icons/gear.png"), b"png").unwrap();
        let entries = vec![AssetEntry {
            path: "assets/img/".into(),
        }];
        let resolved = resolve_assets(&entries, &root).expect("resolve");
        let keys: Vec<&str> = resolved.iter().map(|r| r.bundle_path.as_str()).collect();
        assert_eq!(keys, ["img/icons/gear.png", "img/logo.png"]);
    }

    #[test]
    fn resolves_glob_pattern() {
        let root = tmp_root("glob");
        fs::create_dir_all(root.join("assets/sounds")).unwrap();
        fs::write(root.join("assets/sounds/a.wav"), b"a").unwrap();
        fs::write(root.join("assets/sounds/b.mp3"), b"b").unwrap();
        fs::write(root.join("assets/sounds/c.wav"), b"c").unwrap();
        let entries = vec![AssetEntry {
            path: "assets/**/*.wav".into(),
        }];
        let resolved = resolve_assets(&entries, &root).expect("resolve");
        let keys: Vec<&str> = resolved.iter().map(|r| r.bundle_path.as_str()).collect();
        assert_eq!(keys, ["sounds/a.wav", "sounds/c.wav"]);
    }

    #[test]
    fn missing_file_reports_error() {
        let root = tmp_root("missing");
        let entries = vec![AssetEntry {
            path: "nope.bin".into(),
        }];
        let err = resolve_assets(&entries, &root).expect_err("must fail");
        assert!(matches!(err, AssetResolveError::NotFound { .. }));
    }

    #[test]
    fn empty_glob_match_reports_error() {
        let root = tmp_root("empty-glob");
        fs::create_dir_all(root.join("assets")).unwrap();
        let entries = vec![AssetEntry {
            path: "assets/*.png".into(),
        }];
        let err = resolve_assets(&entries, &root).expect_err("must fail");
        assert!(matches!(err, AssetResolveError::NoMatches { .. }));
    }

    #[test]
    fn dedup_across_entries() {
        let root = tmp_root("dedup");
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::write(root.join("assets/x.txt"), b"x").unwrap();
        let entries = vec![
            AssetEntry {
                path: "assets/x.txt".into(),
            },
            AssetEntry {
                path: "assets/".into(),
            },
        ];
        let resolved = resolve_assets(&entries, &root).expect("resolve");
        assert_eq!(resolved.len(), 1);
    }
}
