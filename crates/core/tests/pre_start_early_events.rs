//! Early events submitted before `Runtime::init` are replayed once the
//! runtime starts. Lives in its own test binary because it installs the
//! process-global runtime.

use istmo_core::{EarlyEventKind, Runtime, RuntimeConfig};

#[test]
fn early_events_before_init_are_replayed_in_order() {
    Runtime::submit_early_event(
        "istmo.test.queue".to_owned(),
        EarlyEventKind::Queue { capacity: 4 },
        b"first".to_vec(),
    )
    .expect("buffer first");
    Runtime::submit_early_event(
        "istmo.test.queue".to_owned(),
        EarlyEventKind::Queue { capacity: 4 },
        b"second".to_vec(),
    )
    .expect("buffer second");
    Runtime::submit_early_event(
        "istmo.test.latest".to_owned(),
        EarlyEventKind::Latest,
        b"latest".to_vec(),
    )
    .expect("buffer latest");

    let init = Runtime::init(RuntimeConfig::inline()).expect("init");
    let rt = init.runtime;

    let rx = rt.early_events().queue("istmo.test.queue", 4).subscribe();
    assert_eq!(rx.try_recv().expect("first"), b"first");
    assert_eq!(rx.try_recv().expect("second"), b"second");
    assert_eq!(
        rt.early_events().latest_slot("istmo.test.latest").peek(),
        Some(b"latest".to_vec())
    );

    Runtime::submit_early_event(
        "istmo.test.queue".to_owned(),
        EarlyEventKind::Queue { capacity: 4 },
        b"live".to_vec(),
    )
    .expect("live");
    assert_eq!(rx.try_recv().expect("live"), b"live");
}
