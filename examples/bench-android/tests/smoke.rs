//! Desktop smoke test: the bench runtime boots and the cells that need
//! no Kotlin peer produce one sample per iteration.

#[cfg(not(target_os = "android"))]
#[test]
fn desktop_cells_produce_samples() {
    use bench_android::cells::Cell;

    let init = istmo::Runtime::init(istmo::RuntimeConfig::inline()).expect("init runtime");
    let init = bench_android::__istmo_configure_runtime(init);
    let rt = init.runtime;
    for cell in [
        Cell::IsolatedRaw,
        Cell::InlineRaw,
        Cell::Typed,
        Cell::FlumePingPong,
        Cell::CondvarPingPong,
        Cell::FutexPingPong,
        Cell::SpinThenPark,
        Cell::ThreadSpawnStart,
        Cell::ClockOverhead,
    ] {
        let run = cell.run(&rt, 50, 64).expect("cell runs");
        assert_eq!(run.samples.len(), 50, "{cell:?}");
    }
    assert!(bench_android::run_iterations(1, 10, 64).expect("legacy inline") > 0);
}
