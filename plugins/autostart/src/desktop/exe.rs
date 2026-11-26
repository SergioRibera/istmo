//! The executable path an autostart entry names: one that survives
//! updates.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What the resolution reads from the process environment, passed in so
/// [`ExeEnv::resolve`] stays pure.
#[derive(Debug, Clone, Default)]
pub(crate) struct ExeEnv {
    /// `std::env::current_exe()`.
    pub(crate) current: PathBuf,
    /// `$APPIMAGE`: the image file, not the mount point the process runs
    /// from.
    pub(crate) appimage: Option<PathBuf>,
    /// `$HOME`.
    pub(crate) home: Option<PathBuf>,
    /// `$USER`.
    pub(crate) user: Option<OsString>,
}

impl ExeEnv {
    /// The environment of this process.
    pub(crate) fn from_process() -> std::io::Result<Self> {
        Ok(Self {
            current: std::env::current_exe()?,
            appimage: std::env::var_os("APPIMAGE").map(PathBuf::from),
            home: std::env::var_os("HOME").map(PathBuf::from),
            user: std::env::var_os("USER"),
        })
    }

    /// The path to register.
    ///
    /// - `$APPIMAGE` when it is an absolute path.
    /// - A `/nix/store` executable changes path on every update: the first
    ///   profile link (`~/.nix-profile/bin`, `/etc/profiles/per-user/$USER/bin`,
    ///   `/run/current-system/sw/bin`) that `canonical` resolves to the same
    ///   file — or to the wrapper script of a `.<name>-wrapped` binary —
    ///   else the store path.
    /// - Otherwise the current executable.
    ///
    /// `canonical` is `std::fs::canonicalize`; `None` for a missing path.
    pub(crate) fn resolve(&self, canonical: impl Fn(&Path) -> Option<PathBuf>) -> PathBuf {
        if let Some(image) = self.appimage.as_ref().filter(|p| p.is_absolute()) {
            return image.clone();
        }
        if !self.current.starts_with("/nix/store") {
            return self.current.clone();
        }
        let Some(file_name) = self.current.file_name().and_then(|n| n.to_str()) else {
            return self.current.clone();
        };
        // nixpkgs `wrapProgram` moves `bin/foo` to `bin/.foo-wrapped` and
        // puts a script at `bin/foo`; the profile links the script.
        let link_name = file_name
            .strip_prefix('.')
            .and_then(|n| n.strip_suffix("-wrapped"))
            .unwrap_or(file_name);
        let Some(target) = canonical(&self.current.with_file_name(link_name)) else {
            return self.current.clone();
        };
        self.profile_dirs()
            .into_iter()
            .map(|dir| dir.join(link_name))
            .find(|link| canonical(link).as_ref() == Some(&target))
            .unwrap_or_else(|| self.current.clone())
    }

    fn profile_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::with_capacity(3);
        if let Some(home) = &self.home {
            dirs.push(home.join(".nix-profile/bin"));
        }
        if let Some(user) = &self.user {
            let mut dir = PathBuf::from("/etc/profiles/per-user");
            dir.push(user);
            dir.push("bin");
            dirs.push(dir);
        }
        dirs.push(PathBuf::from("/run/current-system/sw/bin"));
        dirs
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// `canonical` over a fixed link table: a path resolves to its entry,
    /// or to itself when it is listed as a real file.
    fn fs(links: &[(&str, &str)], files: &[&str]) -> impl Fn(&Path) -> Option<PathBuf> + use<> {
        let links: HashMap<PathBuf, PathBuf> = links
            .iter()
            .map(|(from, to)| (PathBuf::from(from), PathBuf::from(to)))
            .collect();
        let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
        move |path| {
            links
                .get(path)
                .cloned()
                .or_else(|| files.iter().find(|f| f.as_path() == path).cloned())
        }
    }

    fn env(current: &str) -> ExeEnv {
        ExeEnv {
            current: PathBuf::from(current),
            appimage: None,
            home: Some(PathBuf::from("/home/ana")),
            user: Some(OsString::from("ana")),
        }
    }

    #[test]
    fn plain_executable_is_kept() {
        let env = env("/usr/bin/notes");
        assert_eq!(env.resolve(fs(&[], &[])), PathBuf::from("/usr/bin/notes"));
    }

    #[test]
    fn appimage_wins_over_the_mount_point() {
        let mut env = env("/tmp/.mount_NotesX1/usr/bin/notes");
        env.appimage = Some(PathBuf::from("/home/ana/Apps/Notes.AppImage"));
        assert_eq!(
            env.resolve(fs(&[], &[])),
            PathBuf::from("/home/ana/Apps/Notes.AppImage")
        );
        env.appimage = Some(PathBuf::from("relative.AppImage"));
        assert_eq!(
            env.resolve(fs(&[], &[])),
            PathBuf::from("/tmp/.mount_NotesX1/usr/bin/notes")
        );
    }

    #[test]
    fn nix_store_prefers_a_profile_link_to_the_same_file() {
        let store = "/nix/store/abc-notes-1.0/bin/notes";
        let env = env(store);
        let canonical = fs(
            &[
                (
                    "/home/ana/.nix-profile/bin/notes",
                    "/nix/store/old-notes-0.9/bin/notes",
                ),
                ("/etc/profiles/per-user/ana/bin/notes", store),
            ],
            &[store],
        );
        assert_eq!(
            env.resolve(canonical),
            PathBuf::from("/etc/profiles/per-user/ana/bin/notes")
        );
    }

    #[test]
    fn nix_wrapped_binary_matches_its_wrapper() {
        let env = env("/nix/store/abc-notes-1.0/bin/.notes-wrapped");
        let canonical = fs(
            &[(
                "/run/current-system/sw/bin/notes",
                "/nix/store/abc-notes-1.0/bin/notes",
            )],
            &["/nix/store/abc-notes-1.0/bin/notes"],
        );
        assert_eq!(
            env.resolve(canonical),
            PathBuf::from("/run/current-system/sw/bin/notes")
        );
    }

    #[test]
    fn nix_store_without_a_profile_link_keeps_the_store_path() {
        let store = "/nix/store/abc-notes-1.0/bin/notes";
        let env = env(store);
        assert_eq!(env.resolve(fs(&[], &[store])), PathBuf::from(store));
    }
}
