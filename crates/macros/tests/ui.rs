//! Compile-time contract of the macros: every rejected shape fails with
//! a spanned, stable message, and the accepted shapes expand cleanly.
//!
//! Regenerate the `.stderr` snapshots after an intentional message
//! change with `TRYBUILD=overwrite cargo test -p istmo-macros --test ui`.

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/fail/*.rs");
    t.pass("tests/ui/pass/*.rs");
}
