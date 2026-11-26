//! Keeps the shared checklist compiling even when no backend test runs.

mod common;

#[test]
fn the_checklist_harness_builds() {
    let _live = common::live();
}
