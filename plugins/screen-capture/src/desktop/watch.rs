//! Source watches: where a backend's watcher publishes its listings, and
//! how they become `SourceChange`s.

#![cfg_attr(
    not(all(
        target_os = "linux",
        any(
            feature = "linux-wlr-screencopy",
            feature = "linux-ext-image-copy",
            feature = "linux-x11"
        )
    )),
    allow(
        dead_code,
        reason = "the watchers live in the platform backends; a build without one only answers that it cannot watch"
    )
)]

use std::time::Duration;

use crate::types::{
    BackendId, CaptureError, Source, SourceCapture, SourceChange, SourceFilter, SourceKind,
    SourceList,
};

/// How often a waiting watcher looks whether its subscriber left.
pub(crate) const WATCH_POLL: Duration = Duration::from_millis(250);

/// The ready picker-only backends and the kinds they offer: where a
/// listed-only source can be captured.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Pickers(pub(crate) Vec<(BackendId, Vec<SourceKind>)>);

impl Pickers {
    /// Fill in the pickers of every listed-only source.
    pub(crate) fn name(&self, sources: &mut [Source]) {
        for source in sources {
            if let SourceCapture::PickerOnly(backends) = &mut source.capture {
                *backends = self
                    .0
                    .iter()
                    .filter(|(_, kinds)| kinds.contains(&source.kind))
                    .map(|(backend, _)| *backend)
                    .collect();
            }
        }
    }
}

type Item = Result<SourceChange, CaptureError>;

/// One subscriber's watch: the listing it saw last, its filter and the
/// channel to it. A backend's watcher calls [`publish`](Self::publish)
/// with every new listing and stops once it returns `false`.
#[derive(Debug)]
pub(crate) struct SourceFeed {
    tx: flume::Sender<Item>,
    filter: SourceFilter,
    pickers: Pickers,
    last: Option<Vec<Source>>,
}

impl SourceFeed {
    pub(crate) const fn new(
        tx: flume::Sender<Item>,
        filter: SourceFilter,
        pickers: Pickers,
    ) -> Self {
        Self {
            tx,
            filter,
            pickers,
            last: None,
        }
    }

    /// Whether the subscriber left.
    pub(crate) fn is_closed(&self) -> bool {
        self.tx.is_disconnected()
    }

    /// Publish the current listing: all of it the first time (`Listed`),
    /// then only what changed. `false` once the subscriber left.
    pub(crate) fn publish(&mut self, mut list: SourceList) -> bool {
        list.sources.retain(|s| self.filter.admits(s.kind));
        self.pickers.name(&mut list.sources);
        let changes = match &self.last {
            None => vec![SourceChange::Listed(list.clone())],
            Some(seen) => SourceChange::between(seen, &list.sources),
        };
        self.last = Some(list.sources);
        // Blocks while the subscriber's queue is full: that only holds
        // this watch's own thread.
        changes
            .into_iter()
            .all(|change| self.tx.send(Ok(change)).is_ok())
    }

    /// End the watch with `err`.
    pub(crate) fn fail(self, err: CaptureError) {
        // The subscriber may be gone already.
        let _sent = self.tx.send(Err(err));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SourceId, SourceListing};

    fn window(value: &str, title: &str) -> Source {
        let mut source = Source::new(
            SourceId::new(BackendId::LinuxWlrScreencopy, value),
            SourceKind::Window,
            title,
        );
        source.capture = SourceCapture::PickerOnly(Vec::new());
        source
    }

    fn listing(sources: Vec<Source>) -> SourceList {
        SourceList {
            sources,
            listing: SourceListing::Enumerable,
        }
    }

    #[test]
    fn the_first_listing_is_whole_then_only_changes_follow() {
        let (tx, rx) = flume::unbounded();
        let pickers = Pickers(vec![(BackendId::LinuxPortal, vec![SourceKind::Window])]);
        let mut feed = SourceFeed::new(tx, SourceFilter::default(), pickers);
        assert!(feed.publish(listing(vec![window("a", "A")])));
        let Ok(SourceChange::Listed(first)) = rx.try_recv().expect("an item") else {
            panic!("the first item lists everything");
        };
        assert_eq!(
            first.sources[0].capture,
            SourceCapture::PickerOnly(vec![BackendId::LinuxPortal]),
            "pickers are named"
        );
        // Nothing new: nothing sent.
        assert!(feed.publish(listing(vec![window("a", "A")])));
        assert!(rx.try_recv().is_err());
        assert!(feed.publish(listing(vec![window("a", "A2"), window("b", "B")])));
        let changes: Vec<SourceChange> = rx.try_iter().map(|c| c.expect("change")).collect();
        assert!(
            matches!(&changes[..], [SourceChange::Added(b), SourceChange::Changed(a)]
            if b.id.value == "b" && a.name == "A2")
        );
        drop(rx);
        assert!(feed.is_closed());
        assert!(!feed.publish(listing(Vec::new())), "the subscriber left");
    }

    #[test]
    fn the_filter_applies_to_every_listing() {
        let (tx, rx) = flume::unbounded();
        let filter = SourceFilter {
            kinds: vec![SourceKind::Monitor],
            ..SourceFilter::default()
        };
        let mut feed = SourceFeed::new(tx, filter, Pickers::default());
        assert!(feed.publish(listing(vec![window("a", "A")])));
        assert!(feed.publish(listing(vec![window("a", "A"), window("b", "B")])));
        let items: Vec<Item> = rx.try_iter().collect();
        assert_eq!(
            items,
            vec![Ok(SourceChange::Listed(listing(Vec::new())))],
            "windows filtered out never show"
        );
    }
}
