//! Early-event primitive semantics: value-retention for lifecycle-style
//! events, buffered drain for launch-intent-style events.

use istmo_core::early_events::{EarlyEventStore, LatestValueSlot, PreMainQueue};

#[test]
fn latest_value_slot_replays_current_value_to_late_subscribers() {
    let slot = LatestValueSlot::new();
    slot.publish(vec![1]);
    slot.publish(vec![2]);
    let rx = slot.subscribe();
    // Late subscriber immediately sees the retained value.
    assert_eq!(rx.recv().unwrap(), vec![2]);
    slot.publish(vec![3]);
    assert_eq!(rx.recv().unwrap(), vec![3]);
}

#[test]
fn latest_value_slot_broadcasts_to_all_subscribers() {
    let slot = LatestValueSlot::new();
    let rx1 = slot.subscribe();
    let rx2 = slot.subscribe();
    slot.publish(vec![42]);
    assert_eq!(rx1.recv().unwrap(), vec![42]);
    assert_eq!(rx2.recv().unwrap(), vec![42]);
}

#[test]
fn latest_value_slot_peek_returns_current() {
    let slot = LatestValueSlot::new();
    assert!(slot.peek().is_none());
    slot.publish(vec![1, 2]);
    assert_eq!(slot.peek(), Some(vec![1, 2]));
}

#[test]
fn pre_main_queue_drains_buffer_into_first_subscriber() {
    let queue = PreMainQueue::new(8);
    queue.publish(vec![1]);
    queue.publish(vec![2]);
    queue.publish(vec![3]);
    let rx = queue.subscribe();
    assert_eq!(rx.recv().unwrap(), vec![1]);
    assert_eq!(rx.recv().unwrap(), vec![2]);
    assert_eq!(rx.recv().unwrap(), vec![3]);
}

#[test]
fn pre_main_queue_drops_oldest_when_full() {
    let queue = PreMainQueue::new(2);
    queue.publish(vec![1]);
    queue.publish(vec![2]);
    queue.publish(vec![3]);
    let rx = queue.subscribe();
    assert_eq!(rx.recv().unwrap(), vec![2]);
    assert_eq!(rx.recv().unwrap(), vec![3]);
    assert!(rx.try_recv().is_err());
}

#[test]
fn pre_main_queue_forwards_live_events_after_first_subscriber() {
    let queue = PreMainQueue::new(4);
    queue.publish(vec![1]);
    let rx = queue.subscribe();
    assert_eq!(rx.recv().unwrap(), vec![1]);
    queue.publish(vec![2]);
    assert_eq!(rx.recv().unwrap(), vec![2]);
}

#[test]
fn pre_main_queue_second_subscriber_gets_no_backlog() {
    let queue = PreMainQueue::new(4);
    queue.publish(vec![1]);
    let rx1 = queue.subscribe();
    let rx2 = queue.subscribe();
    // Backlog was drained into rx1 only.
    assert_eq!(rx1.recv().unwrap(), vec![1]);
    assert!(rx2.try_recv().is_err());
    queue.publish(vec![2]);
    assert_eq!(rx1.recv().unwrap(), vec![2]);
    assert_eq!(rx2.recv().unwrap(), vec![2]);
}

#[test]
fn early_event_store_reuses_slots_by_key() {
    let store = EarlyEventStore::new();
    let a1 = store.latest_slot("lifecycle");
    let a2 = store.latest_slot("lifecycle");
    a1.publish(vec![7]);
    assert_eq!(a2.peek(), Some(vec![7]));
    // A different key yields a different slot.
    let b = store.latest_slot("deep-links");
    assert!(b.peek().is_none());
}

#[test]
fn early_event_store_reuses_queues_by_key() {
    let store = EarlyEventStore::new();
    let q1 = store.queue("push", 4);
    let q2 = store.queue("push", 999);
    q1.publish(vec![1]);
    let rx = q2.subscribe();
    assert_eq!(rx.recv().unwrap(), vec![1]);
}
