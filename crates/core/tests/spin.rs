//! The process-wide spin switch (its own test binary: it flips global
//! state other tests read).

use istmo_core::spin::{SpinWait, set_enabled};

#[test]
fn disabled_spinning_parks_at_once() {
    let site = SpinWait::new();
    set_enabled(false);
    let mut polled = false;
    let got = site.spin(|| {
        polled = true;
        Some(())
    });
    set_enabled(true);
    assert_eq!(got, None);
    assert!(!polled);
}
