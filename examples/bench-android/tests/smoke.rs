//! Smoke test: desktop host can boot the bench runtime and get sensible
//! per-iter timings. Confirms the harness isn't broken before we build
//! it for Android.

#[cfg(not(target_os = "android"))]
#[test]
fn desktop_smoke_runs_all_modes() {
    bench_android::smoke();
}
