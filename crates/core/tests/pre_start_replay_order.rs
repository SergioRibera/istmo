//! Early events submitted while `Runtime::init` runs keep their order:
//! every buffered event is replayed before any event delivered live.
//! Own test binary because it installs the process-global runtime.

use std::thread;

use istmo_core::{EarlyEventKind, Runtime, RuntimeConfig};

const CHANNEL: &str = "istmo.test.race";
const EVENTS: u32 = 2_000;

#[test]
fn events_racing_init_arrive_in_submission_order() {
    let producer = thread::spawn(|| {
        for i in 0..EVENTS {
            Runtime::submit_early_event(
                CHANNEL.to_owned(),
                EarlyEventKind::Queue { capacity: 1024 },
                i.to_le_bytes().to_vec(),
            )
            .expect("submit");
        }
    });
    thread::yield_now();
    let init = Runtime::init(RuntimeConfig::inline()).expect("init");
    producer.join().expect("producer");

    let received: Vec<u32> = init
        .runtime
        .early_events()
        .queue(CHANNEL, 1024)
        .subscribe()
        .drain()
        .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four bytes")))
        .collect();
    assert_eq!(received.last(), Some(&(EVENTS - 1)));
    assert!(
        received.windows(2).all(|pair| pair[0] < pair[1]),
        "events out of order: {received:?}"
    );
}
