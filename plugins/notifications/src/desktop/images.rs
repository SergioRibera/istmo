//! Image files for backends that take paths.
//!
//! PNG / JPEG bytes are written once to the cache directory under a
//! content-hash name (`<fnv1a-64 hex>-<len>.<ext>`), shared by every
//! notification that uses the same bytes, and removed when the last one
//! using them is replaced, cancelled or dismissed. At start, files older
//! than [`STALE_AFTER`] are removed: they belong to notifications of an
//! earlier process (the platform may still show those, so younger files
//! stay).

use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::backend::{ImageFiles, ImageSlot};
use crate::{Notification, NotificationImage, Style};

/// Age past which a file left by an earlier process is removed.
pub(crate) const STALE_AFTER: Duration = Duration::from_secs(3 * 24 * 3_600);

/// FNV-1a, 64 bits: stable across builds, so names survive restarts.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The cache directory and how many shown notifications use each file.
#[derive(Debug)]
pub(crate) struct ImageCache {
    dir: PathBuf,
    uses: HashMap<PathBuf, usize>,
}

impl ImageCache {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            uses: HashMap::new(),
        }
    }

    /// Removes files older than [`STALE_AFTER`] that nothing uses.
    pub(crate) fn sweep(&self, now: SystemTime) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if self.uses.contains_key(&path) {
                continue;
            }
            let stale = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age >= STALE_AFTER);
            if stale && let Err(err) = fs::remove_file(&path) {
                tracing::debug!(%err, path = %path.display(), "notifications: stale image not removed");
            }
        }
    }

    /// The file of `bytes`, written when no one uses it yet.
    fn acquire(&mut self, bytes: &[u8], ext: &str) -> std::io::Result<PathBuf> {
        let name = format!("{:016x}-{}.{ext}", fnv1a(bytes), bytes.len());
        let path = self.dir.join(name);
        if let Some(uses) = self.uses.get_mut(&path) {
            *uses += 1;
            return Ok(path);
        }
        if !path.is_file() {
            fs::create_dir_all(&self.dir)?;
            let mut tmp = path.clone().into_os_string();
            tmp.push(".tmp");
            let tmp = PathBuf::from(tmp);
            let written = fs::File::create(&tmp).and_then(|mut file| file.write_all(bytes));
            if let Err(err) = written.and_then(|()| fs::rename(&tmp, &path)) {
                let _cleanup = fs::remove_file(&tmp);
                return Err(err);
            }
        }
        self.uses.insert(path.clone(), 1);
        Ok(path)
    }

    /// One use of `path` less; the file goes with the last one.
    fn release_path(&mut self, path: &Path) {
        let Some(uses) = self.uses.get_mut(path) else {
            return;
        };
        *uses -= 1;
        if *uses == 0 {
            self.uses.remove(path);
            if let Err(err) = fs::remove_file(path) {
                tracing::debug!(%err, path = %path.display(), "notifications: image not removed");
            }
        }
    }

    /// Releases every cached file of `files` (paths given by the app are
    /// left alone).
    pub(crate) fn release(&mut self, files: &ImageFiles) {
        for (_, path) in &files.entries {
            if path.starts_with(&self.dir) {
                self.release_path(path);
            }
        }
    }

    fn slot(&mut self, files: &mut ImageFiles, slot: ImageSlot, image: &NotificationImage) {
        let path = match image {
            NotificationImage::Png(bytes) => self.acquire(bytes, "png"),
            NotificationImage::Jpeg(bytes) => self.acquire(bytes, "jpg"),
            NotificationImage::File(path) => Ok(PathBuf::from(path)),
            NotificationImage::Named(_) => return,
        };
        match path {
            Ok(path) => files.entries.push((slot, path)),
            Err(err) => tracing::warn!(%err, "notifications: image not written; shown without it"),
        }
    }

    /// The files of every image of `n`.
    pub(crate) fn collect(&mut self, n: &Notification) -> ImageFiles {
        let mut files = ImageFiles::default();
        if let Some(icon) = &n.icon {
            self.slot(&mut files, ImageSlot::Icon, icon);
        }
        let avatar = |cache: &mut Self, files: &mut ImageFiles, who: &crate::Participant| {
            let slot = ImageSlot::Avatar(who.key.clone());
            if let Some(image) = &who.avatar
                && files.get(&slot).is_none()
            {
                cache.slot(files, slot, image);
            }
        };
        match &n.style {
            Style::Picture(picture) => self.slot(&mut files, ImageSlot::Picture, picture),
            Style::Messaging(conversation) => {
                avatar(self, &mut files, &conversation.user);
                for (index, message) in conversation.messages.iter().enumerate() {
                    if let Some(sender) = &message.sender {
                        avatar(self, &mut files, sender);
                    }
                    if let Some(image) = &message.image {
                        self.slot(&mut files, ImageSlot::Message(index), image);
                    }
                }
            }
            Style::Call(call) => avatar(self, &mut files, &call.caller),
            Style::Plain | Style::BigText(_) | Style::Inbox(_) => {}
        }
        for action in &n.actions {
            if let Some(icon) = &action.icon {
                self.slot(&mut files, ImageSlot::Action(action.id.clone()), icon);
            }
        }
        files
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, Conversation, Participant};

    fn dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("istmo-notif-img-{name}-{}", std::process::id()))
    }

    #[test]
    fn files_are_shared_by_content_and_removed_with_the_last_use() {
        let root = dir("share");
        let mut cache = ImageCache::new(root.clone());
        let png = NotificationImage::png(vec![1, 2, 3]);
        let a = Notification::new("a", "A").unwrap().with_icon(png.clone());
        let b = Notification::new("b", "B")
            .unwrap()
            .with_action(Action::button("x", "X").unwrap().with_icon(png));
        let files_a = cache.collect(&a);
        let files_b = cache.collect(&b);
        let path = files_a.get(&ImageSlot::Icon).unwrap().to_path_buf();
        assert_eq!(
            files_b.get(&ImageSlot::Action(crate::ActionId::try_from("x").unwrap())),
            Some(path.as_path())
        );
        assert_eq!(fs::read(&path).unwrap(), [1, 2, 3]);
        cache.release(&files_a);
        assert!(path.exists());
        cache.release(&files_b);
        assert!(!path.exists());
        let _cleanup = fs::remove_dir_all(root);
    }

    #[test]
    fn avatars_are_written_once_per_participant() {
        let root = dir("avatar");
        let mut cache = ImageCache::new(root.clone());
        let ana = Participant::new("ana", "Ana").with_avatar(NotificationImage::png(vec![9]));
        let me = Participant::new("me", "Me").with_avatar(NotificationImage::file("/tmp/me.png"));
        let chat = Notification::new("c", "Chat")
            .unwrap()
            .with_style(Style::Messaging(
                Conversation::new(me)
                    .with_message(crate::ChatMessage::received(ana.clone(), "hi"))
                    .with_message(crate::ChatMessage::received(ana, "there")),
            ));
        let files = cache.collect(&chat);
        assert_eq!(files.entries.len(), 2);
        assert_eq!(
            files.get(&ImageSlot::Avatar("me".to_owned())),
            Some(Path::new("/tmp/me.png"))
        );
        cache.release(&files);
        // The app's own file is never touched.
        assert!(cache.uses.is_empty());
        let _cleanup = fs::remove_dir_all(root);
    }

    #[test]
    fn sweep_removes_only_old_unused_files() {
        let root = dir("sweep");
        fs::create_dir_all(&root).unwrap();
        let old = root.join("old.png");
        fs::write(&old, [0]).unwrap();
        let cache = ImageCache::new(root.clone());
        cache.sweep(SystemTime::now());
        assert!(old.exists());
        cache.sweep(SystemTime::now() + STALE_AFTER + Duration::from_secs(1));
        assert!(!old.exists());
        let _cleanup = fs::remove_dir_all(root);
    }
}
