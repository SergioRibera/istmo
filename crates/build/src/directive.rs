//! Cargo build-script directives (`cargo:rerun-if-changed=…`,
//! `cargo::warning=…`) printed by the emit pipeline.
//!
//! Routing them through one place lets unit tests capture exactly what
//! a run would have told Cargo, instead of scraping stdout.

use std::cell::RefCell;
use std::path::Path;

thread_local! {
    /// `Some` while [`capture`] runs on this thread.
    static CAPTURED: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// Print one directive line, or record it while a capture is active.
pub(crate) fn emit(line: String) {
    let line = CAPTURED.with_borrow_mut(|captured| match captured {
        Some(lines) => {
            lines.push(line);
            None
        }
        None => Some(line),
    });
    if let Some(line) = line {
        println!("{line}");
    }
}

/// `cargo:rerun-if-changed=<path>`.
pub(crate) fn rerun_if_changed(path: &Path) {
    emit(format!("cargo:rerun-if-changed={}", path.display()));
}

/// [`rerun_if_changed`] for an input that may not exist. Cargo treats a
/// missing tracked path as always stale, so absent paths are skipped.
pub(crate) fn rerun_if_exists(path: &Path) {
    if path.exists() {
        rerun_if_changed(path);
    }
}

/// `cargo:rerun-if-env-changed=<var>`.
pub(crate) fn rerun_if_env_changed(var: &str) {
    emit(format!("cargo:rerun-if-env-changed={var}"));
}

/// `cargo::warning=istmo-build: <message>`.
pub(crate) fn warning(message: &str) {
    emit(format!("cargo::warning=istmo-build: {message}"));
}

/// Run `f`, returning its result together with every directive it
/// emitted on this thread instead of printing them.
#[cfg(test)]
pub(crate) fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    let previous = CAPTURED.replace(Some(Vec::new()));
    let result = f();
    let lines = CAPTURED.replace(previous).unwrap_or_default();
    (result, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_collects_instead_of_printing() {
        let ((), lines) = capture(|| {
            rerun_if_changed(Path::new("build.rs"));
            warning("careful");
        });
        assert_eq!(
            lines,
            [
                "cargo:rerun-if-changed=build.rs",
                "cargo::warning=istmo-build: careful",
            ]
        );
    }

    #[test]
    fn missing_paths_are_not_tracked() {
        let ((), lines) = capture(|| rerun_if_exists(Path::new("/definitely/not/here")));
        assert_eq!(lines, Vec::<String>::new());
    }
}
