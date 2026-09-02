//! Declarative native-dependency model.
//!
//! A plugin frequently pulls in a native library the app builder must
//! surface to Gradle (Android) or `SwiftPM` / Xcode (iOS). Examples the M6
//! Google Sign-In plugin needs:
//!
//! * `androidx.credentials:credentials:1.3.0`
//! * `androidx.credentials:credentials-play-services-auth:1.3.0`
//! * `com.google.android.libraries.identity.googleid:googleid:1.1.1`
//!
//! Multiple plugins in the same app can request the same coordinate under
//! different versions. This module models the metadata and provides a merge
//! step whose policy is *highest wins, warn on downgrade candidates* — the
//! same policy the PLAN documents. Downstream consumers (a `build.rs` script
//! collecting metadata from each plugin's `istmo.toml`) call [`NativeDeps::merge`]
//! for each contributor and then emit the Gradle fragment.
//!
//! iOS side is modelled but not rendered yet — SPM's `Package.swift` is not
//! a fragment-friendly format, so integration lands with the future iOS
//! build-integration recipe.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Scope a Gradle dependency is added under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GradleScope {
    /// `implementation` — main sources.
    Implementation,
    /// `api` — exposed on the module's compile classpath transitively.
    Api,
    /// `runtimeOnly` — packaged, not on the compile classpath.
    RuntimeOnly,
    /// `compileOnly` — on the compile classpath, not packaged.
    CompileOnly,
}

impl GradleScope {
    #[must_use]
    pub const fn as_gradle_str(self) -> &'static str {
        match self {
            Self::Implementation => "implementation",
            Self::Api => "api",
            Self::RuntimeOnly => "runtimeOnly",
            Self::CompileOnly => "compileOnly",
        }
    }
}

/// A Maven / Gradle coordinate: `group:artifact:version`.
///
/// Versions are compared lexicographically per dot-separated segment with
/// numeric-when-possible ordering. That gets `1.2.10` above `1.2.9`, which
/// naïve string comparison does not, and covers what real Android artefact
/// versions actually look like without pulling in a full semver crate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GradleCoord {
    pub group: String,
    pub artifact: String,
    pub version: String,
}

impl GradleCoord {
    #[must_use]
    pub fn new(
        group: impl Into<String>,
        artifact: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            group: group.into(),
            artifact: artifact.into(),
            version: version.into(),
        }
    }

    /// The coordinate without its version, used to detect duplicates on
    /// merge.
    #[must_use]
    pub fn key(&self) -> GradleKey {
        GradleKey {
            group: self.group.clone(),
            artifact: self.artifact.clone(),
        }
    }

    /// Render as the string Gradle accepts inside its DSL:
    /// `"group:artifact:version"`.
    #[must_use]
    pub fn as_notation(&self) -> String {
        format!("{}:{}:{}", self.group, self.artifact, self.version)
    }
}

/// Group + artifact pair — the identity used to dedupe versions on merge.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GradleKey {
    pub group: String,
    pub artifact: String,
}

/// One entry in [`NativeDeps::gradle`]: a coordinate at a specific scope.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GradleDep {
    pub scope: GradleScope,
    pub coord: GradleCoord,
}

impl GradleDep {
    #[must_use]
    pub const fn new(scope: GradleScope, coord: GradleCoord) -> Self {
        Self { scope, coord }
    }
}

/// A Swift package product a plugin needs to link against.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SwiftPackageDep {
    /// Package URL (e.g. `https://github.com/google/GoogleSignIn-iOS.git`).
    pub url: String,
    /// Product name inside the package (`.product(name: ...)`).
    pub product: String,
    /// Minimum version requirement, rendered as `from:` in `Package.swift`.
    pub from_version: String,
}

/// Warning emitted by [`NativeDeps::merge`] when two contributors disagreed
/// on a coordinate's version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionConflict {
    pub key: GradleKey,
    pub picked: String,
    pub discarded: Vec<String>,
}

/// Aggregate of native dependencies contributed by one or more plugins.
///
/// Constructed empty ([`Self::new`] / [`Default`]), populated with
/// [`Self::add_gradle`] / [`Self::add_swift_package`], and combined across
/// plugins via [`Self::merge`]. The renderers ([`Self::render_gradle`]) then
/// produce fragments ready to splice into a real Gradle build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeDeps {
    /// Keyed by `(scope, group, artifact)` so different scopes of the same
    /// artefact (rare, but legal) stay distinct while merges dedupe on
    /// coordinate key.
    gradle: BTreeMap<(GradleScope, GradleKey), String>,
    swift: BTreeMap<(String, String), String>,
    conflicts: Vec<VersionConflict>,
}

impl NativeDeps {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds (or replaces at higher version) a Gradle dependency.
    pub fn add_gradle(&mut self, dep: &GradleDep) -> &mut Self {
        let key = (dep.scope, dep.coord.key());
        if let Some(existing) = self.gradle.get_mut(&key) {
            let picked = pick_highest(existing, &dep.coord.version);
            if picked != *existing {
                self.conflicts.push(VersionConflict {
                    key: dep.coord.key(),
                    picked: picked.clone(),
                    discarded: vec![existing.clone()],
                });
                *existing = picked;
            } else if picked != dep.coord.version {
                self.conflicts.push(VersionConflict {
                    key: dep.coord.key(),
                    picked,
                    discarded: vec![dep.coord.version.clone()],
                });
            }
        } else {
            self.gradle.insert(key, dep.coord.version.clone());
        }
        self
    }

    /// Adds a Swift package requirement. Duplicate `(url, product)` pairs
    /// resolve to the highest requested `from_version`, same policy as
    /// Gradle.
    pub fn add_swift_package(&mut self, dep: &SwiftPackageDep) -> &mut Self {
        let key = (dep.url.clone(), dep.product.clone());
        let entry = self
            .swift
            .entry(key)
            .or_insert_with(|| dep.from_version.clone());
        let picked = pick_highest(entry, &dep.from_version);
        *entry = picked;
        self
    }

    /// Merges every dependency from `other` into `self`, applying the
    /// version conflict policy per entry. Handy for aggregating multiple
    /// plugins into a single set.
    pub fn merge(&mut self, other: Self) -> &mut Self {
        for ((scope, key), version) in other.gradle {
            self.add_gradle(&GradleDep {
                scope,
                coord: GradleCoord {
                    group: key.group,
                    artifact: key.artifact,
                    version,
                },
            });
        }
        for ((url, product), from_version) in other.swift {
            self.add_swift_package(&SwiftPackageDep {
                url,
                product,
                from_version,
            });
        }
        self.conflicts.extend(other.conflicts);
        self
    }

    /// All Gradle entries in stable render order.
    pub fn gradle_entries(&self) -> impl Iterator<Item = GradleDep> + '_ {
        self.gradle.iter().map(|((scope, key), version)| GradleDep {
            scope: *scope,
            coord: GradleCoord {
                group: key.group.clone(),
                artifact: key.artifact.clone(),
                version: version.clone(),
            },
        })
    }

    /// All Swift package entries in stable render order.
    pub fn swift_entries(&self) -> impl Iterator<Item = SwiftPackageDep> + '_ {
        self.swift
            .iter()
            .map(|((url, product), from_version)| SwiftPackageDep {
                url: url.clone(),
                product: product.clone(),
                from_version: from_version.clone(),
            })
    }

    /// Version-conflict warnings produced by successive [`Self::add_gradle`]
    /// / [`Self::merge`] calls. Consumers should surface them from their
    /// `build.rs` via `cargo:warning=…` so the human sees the downgrade.
    #[must_use]
    pub fn conflicts(&self) -> &[VersionConflict] {
        &self.conflicts
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.gradle.is_empty() && self.swift.is_empty()
    }

    /// Renders a Gradle Kotlin-DSL `dependencies { ... }` block ready to be
    /// spliced into an app module's `build.gradle.kts`. Deterministic output
    /// — entries appear in `(scope, group, artifact)` order.
    #[must_use]
    pub fn render_gradle(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "// GENERATED by istmo-build - DO NOT EDIT.");
        let _ = writeln!(out, "// Aggregate native dependencies.");
        let _ = writeln!(out, "dependencies {{");
        for dep in self.gradle_entries() {
            let _ = writeln!(
                out,
                "    {scope}(\"{notation}\")",
                scope = dep.scope.as_gradle_str(),
                notation = dep.coord.as_notation(),
            );
        }
        let _ = writeln!(out, "}}");
        out
    }
}

/// Pick the higher of two version strings under a segment-wise, numeric-aware
/// comparison. The comparison keeps two goals: (1) `1.2.10` > `1.2.9`, and
/// (2) letters compare as strings (`1.0.0-beta` < `1.0.0`), which matches the
/// intuition Maven's own resolution has for the common cases.
fn pick_highest(existing: &str, candidate: &str) -> String {
    if compare_versions(candidate, existing).is_gt() {
        candidate.to_owned()
    } else {
        existing.to_owned()
    }
}

fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let (a_core, a_pre) = split_pre_release(a);
    let (b_core, b_pre) = split_pre_release(b);
    match compare_dotted(a_core, b_core) {
        std::cmp::Ordering::Equal => {
            // Pre-release qualifier: no qualifier ranks *higher* than any
            // qualifier (semver-style: `1.0.0` > `1.0.0-beta`), and two
            // qualifiers compare lexicographically as a good-enough fallback.
            match (a_pre, b_pre) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (Some(ap), Some(bp)) => ap.cmp(bp),
            }
        }
        other => other,
    }
}

fn split_pre_release(v: &str) -> (&str, Option<&str>) {
    v.split_once('-')
        .map_or((v, None), |(core, pre)| (core, Some(pre)))
}

fn compare_dotted(a: &str, b: &str) -> std::cmp::Ordering {
    let mut a_parts = a.split('.');
    let mut b_parts = b.split('.');
    loop {
        match (a_parts.next(), b_parts.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(ap), Some(bp)) => {
                let ord = match (ap.parse::<u64>(), bp.parse::<u64>()) {
                    (Ok(an), Ok(bn)) => an.cmp(&bn),
                    _ => ap.cmp(bp),
                };
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_picks_highest_version_and_records_conflict() {
        let mut a = NativeDeps::new();
        a.add_gradle(&GradleDep::new(
            GradleScope::Implementation,
            GradleCoord::new("group", "art", "1.2.9"),
        ));
        let mut b = NativeDeps::new();
        b.add_gradle(&GradleDep::new(
            GradleScope::Implementation,
            GradleCoord::new("group", "art", "1.2.10"),
        ));
        a.merge(b);
        let entries: Vec<_> = a.gradle_entries().collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].coord.version, "1.2.10");
        assert_eq!(a.conflicts().len(), 1);
        assert_eq!(a.conflicts()[0].picked, "1.2.10");
        assert_eq!(a.conflicts()[0].discarded, vec!["1.2.9".to_owned()]);
    }

    #[test]
    fn merge_ignores_older_candidate_without_dropping_current() {
        let mut a = NativeDeps::new();
        a.add_gradle(&GradleDep::new(
            GradleScope::Implementation,
            GradleCoord::new("group", "art", "2.0.0"),
        ));
        a.add_gradle(&GradleDep::new(
            GradleScope::Implementation,
            GradleCoord::new("group", "art", "1.9.9"),
        ));
        let entries: Vec<_> = a.gradle_entries().collect();
        assert_eq!(entries[0].coord.version, "2.0.0");
        assert_eq!(a.conflicts().len(), 1);
        assert_eq!(a.conflicts()[0].picked, "2.0.0");
        assert_eq!(a.conflicts()[0].discarded, vec!["1.9.9".to_owned()]);
    }

    #[test]
    fn distinct_artifacts_coexist() {
        let mut a = NativeDeps::new();
        a.add_gradle(&GradleDep::new(
            GradleScope::Implementation,
            GradleCoord::new("g", "art-a", "1.0.0"),
        ));
        a.add_gradle(&GradleDep::new(
            GradleScope::Api,
            GradleCoord::new("g", "art-b", "0.1.0"),
        ));
        assert_eq!(a.gradle_entries().count(), 2);
        assert!(a.conflicts().is_empty());
    }

    #[test]
    fn compare_versions_orders_numeric_segments_by_value() {
        assert_eq!(
            compare_versions("1.2.10", "1.2.9"),
            std::cmp::Ordering::Greater,
        );
        assert_eq!(
            compare_versions("1.0.0", "1.0.0-beta"),
            std::cmp::Ordering::Greater,
        );
        assert_eq!(
            compare_versions("2.0.0", "1.9.9"),
            std::cmp::Ordering::Greater,
        );
    }
}
