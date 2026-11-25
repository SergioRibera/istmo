//! Hands the web view contract and manifest to the app build; with
//! `linux-qt` on Linux, also builds the Qt `WebEngine` shim.
fn main() {
    istmo_build::emit();
    #[cfg(feature = "linux-qt")]
    qt::build();
}

#[cfg(feature = "linux-qt")]
mod qt {
    use std::path::PathBuf;

    const SOURCE: &str = "native/qt/istmo_webview_qt.cpp";

    /// Compile the shim to `$OUT_DIR/libistmo_webview_qt.so` and hand its
    /// path to the crate as `ISTMO_WEBVIEW_QT_SHIM`. Without Qt
    /// development files the build goes on: the engine then looks for a
    /// shim next to the executable at run time.
    pub(super) fn build() {
        println!("cargo::rerun-if-changed={SOURCE}");
        println!("cargo::rerun-if-env-changed=PKG_CONFIG_PATH");
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
            return;
        }
        let probe = |name: &str| pkg_config::Config::new().cargo_metadata(false).probe(name);
        let libs = match (probe("Qt6WebEngineWidgets"), probe("Qt6Network")) {
            (Ok(web), Ok(net)) => [web, net],
            (Err(err), _) | (_, Err(err)) => {
                println!(
                    "cargo::warning=istmo-webview: Qt WebEngine shim not built ({err}); ship libistmo_webview_qt.so next to the app"
                );
                return;
            }
        };
        let out =
            PathBuf::from(std::env::var_os("OUT_DIR").expect("invariant: cargo sets OUT_DIR"))
                .join("libistmo_webview_qt.so");
        let compiler = cc::Build::new().cpp(true).get_compiler();
        let mut cmd = compiler.to_command();
        cmd.args([
            "-std=c++17",
            "-shared",
            "-fPIC",
            "-O2",
            "-fvisibility=hidden",
        ]);
        for lib in &libs {
            for dir in &lib.include_paths {
                cmd.arg("-I").arg(dir);
            }
        }
        cmd.arg(SOURCE).arg("-o").arg(&out);
        for lib in &libs {
            for dir in &lib.link_paths {
                // The shim finds the Qt it was built against first.
                cmd.arg("-L")
                    .arg(dir)
                    .arg(format!("-Wl,-rpath,{}", dir.display()));
            }
            for name in &lib.libs {
                cmd.arg(format!("-l{name}"));
            }
        }
        match cmd.status() {
            Ok(status) if status.success() => {
                println!("cargo::rustc-env=ISTMO_WEBVIEW_QT_SHIM={}", out.display());
            }
            Ok(status) => {
                println!("cargo::warning=istmo-webview: Qt shim compile failed ({status})");
            }
            Err(err) => {
                println!("cargo::warning=istmo-webview: no C++ compiler for the Qt shim ({err})");
            }
        }
    }
}
