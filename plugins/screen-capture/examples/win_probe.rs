//! The Windows probe: runs the per-backend checklist of
//! `design/screen-capture.md` against the live desktop, through the
//! public API, and prints a PASS / FAIL / WARN / SKIP report (also
//! written to `win_probe_report.txt` in the working directory).
//!
//! - every compiled backend: availability, the capability report, the
//!   source listing, CPU screenshots (one monitor, all monitors), a GPU
//!   screenshot, a 5 s stream, the CPU, shareable and window paths,
//!   exhaustion (`Stall`, `CopyToCpu`), cursor metadata, a sink dropped
//!   mid-stream;
//! - `watch_sources` against a window the probe opens and closes;
//! - audio: system loopback, process loopback (exclude self, one app),
//!   and a video session with audio;
//! - the borderless grant (read, never prompted) and the origin rule.
//!
//! Window titles are never printed. The probe animates a small window of
//! its own (PowerShell + WinForms) so damage-driven producers have
//! something to deliver.
//!
//! Build: `cargo build --release -p istmo-screen-capture --example win_probe
//! --target x86_64-pc-windows-msvc`.

#[cfg(windows)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "`pub(crate)` marks crate-internal items; plain `pub` in private modules trips `unreachable_pub`"
)]
mod probe {
    use std::fmt::Write as _;
    use std::io::Write as _;
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use istmo_core::{
        CallOrigin, CancelToken, Dispatch, NativeFrame, Outcome, Overflow, Runtime, RuntimeConfig,
        RuntimeInit, SinkPolicy, StreamItem, codec,
    };
    use istmo_screen_capture::{
        AccessScope, AudioConfig, AudioSource, Availability, BackendCaps, BackendId,
        BackendSelection, BufferMemory, CapabilityQuery, CaptureError, CaptureEvent, CopyStage,
        CursorMode, CursorRequest, D3d11Frame, DesktopCapture, Exhaustion, FormatChoice,
        FormatRequest, FormatTier, FrameMeta, FrameRate, GpuIdentity, GpuPreference, PixelFormat,
        PixelLayout, SampleFormat, ScreenCaptureClient, ScreenCaptureClientExt, ScreenCaptureHost,
        ScreenshotRequest, SessionConfig, SessionHandle, Source, SourceChange, SourceFilter,
        SourceKind, SourceSelector, Strength,
    };

    const BACKENDS: [BackendId; 3] = [
        BackendId::WindowsWgc,
        BackendId::WindowsDxgi,
        BackendId::WindowsGdi,
    ];
    const ANIMATOR_TITLE: &str = "istmo-probe-animator";
    const WATCH_TITLE: &str = "istmo-probe-watch";
    /// Longest a single check may run before it counts as hung.
    const CHECK_LIMIT: Duration = Duration::from_secs(45);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Verdict {
        Pass,
        Fail,
        Warn,
        Skip,
        Info,
    }

    impl Verdict {
        const fn tag(self) -> &'static str {
            match self {
                Self::Pass => "PASS",
                Self::Fail => "FAIL",
                Self::Warn => "WARN",
                Self::Skip => "SKIP",
                Self::Info => "INFO",
            }
        }
    }

    /// Prints as it goes and keeps everything for the report file.
    #[derive(Debug, Default)]
    pub(crate) struct Report {
        text: Mutex<String>,
        counts: Mutex<[u32; 5]>,
    }

    impl Report {
        fn line(&self, line: &str) {
            println!("{line}");
            let _flushed = std::io::stdout().flush();
            let mut text = self.text.lock().unwrap_or_else(PoisonError::into_inner);
            text.push_str(line);
            text.push('\n');
        }

        fn verdict(&self, verdict: Verdict, check: &str, detail: impl std::fmt::Display) {
            let index = match verdict {
                Verdict::Pass => 0,
                Verdict::Fail => 1,
                Verdict::Warn => 2,
                Verdict::Skip => 3,
                Verdict::Info => 4,
            };
            self.counts.lock().unwrap_or_else(PoisonError::into_inner)[index] += 1;
            self.line(&format!("[{}] {check}: {detail}", verdict.tag()));
        }

        fn pass(&self, check: &str, detail: impl std::fmt::Display) {
            self.verdict(Verdict::Pass, check, detail);
        }

        fn fail(&self, check: &str, detail: impl std::fmt::Display) {
            self.verdict(Verdict::Fail, check, detail);
        }

        fn warn(&self, check: &str, detail: impl std::fmt::Display) {
            self.verdict(Verdict::Warn, check, detail);
        }

        fn skip(&self, check: &str, detail: impl std::fmt::Display) {
            self.verdict(Verdict::Skip, check, detail);
        }

        fn info(&self, check: &str, detail: impl std::fmt::Display) {
            self.verdict(Verdict::Info, check, detail);
        }

        fn check(&self, ok: bool, check: &str, detail: impl std::fmt::Display) {
            if ok {
                self.pass(check, detail);
            } else {
                self.fail(check, detail);
            }
        }

        fn section(&self, title: &str) {
            self.line("");
            self.line(&format!("=== {title} ==="));
        }

        fn failures(&self) -> u32 {
            self.counts.lock().unwrap_or_else(PoisonError::into_inner)[1]
        }

        fn summary(&self) -> String {
            let c = *self.counts.lock().unwrap_or_else(PoisonError::into_inner);
            format!(
                "PASS {}  FAIL {}  WARN {}  SKIP {}  INFO {}",
                c[0], c[1], c[2], c[3], c[4]
            )
        }

        fn save(&self, path: &str) {
            let text = self
                .text
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            match std::fs::write(path, text) {
                Ok(()) => println!("report written to {path}"),
                Err(err) => println!("could not write {path}: {err}"),
            }
        }
    }

    #[derive(Clone)]
    struct Probe {
        runtime: Arc<Runtime>,
        client: Arc<ScreenCaptureClient>,
        report: Arc<Report>,
    }

    /// The calling thread's DPI awareness, as the probe's host runs.
    #[allow(unsafe_code, reason = "two argument-free Win32 queries")]
    fn dpi_awareness() -> &'static str {
        use windows::Win32::UI::HiDpi::{
            DPI_AWARENESS_PER_MONITOR_AWARE, DPI_AWARENESS_SYSTEM_AWARE, DPI_AWARENESS_UNAWARE,
            GetAwarenessFromDpiAwarenessContext, GetThreadDpiAwarenessContext,
        };
        // SAFETY: no arguments; returns this thread's context handle.
        let context = unsafe { GetThreadDpiAwarenessContext() };
        // SAFETY: a context handle the OS just returned.
        match unsafe { GetAwarenessFromDpiAwarenessContext(context) } {
            DPI_AWARENESS_UNAWARE => "unaware",
            DPI_AWARENESS_SYSTEM_AWARE => "system aware",
            DPI_AWARENESS_PER_MONITOR_AWARE => "per-monitor aware",
            _ => "unknown",
        }
    }

    fn block<F: std::future::Future>(future: F) -> F::Output {
        pollster::block_on(future)
    }

    fn now_us() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
    }

    const fn fixed(backend: BackendId) -> BackendSelection {
        BackendSelection::Fixed(backend)
    }

    fn texture_tier(format: PixelFormat) -> FormatTier {
        FormatTier::new(vec![FormatChoice {
            format,
            memory: BufferMemory::D3d11Texture,
            modifiers: Vec::new(),
        }])
    }

    /// Run `check` on a thread of its own; a check that does not return
    /// within [`CHECK_LIMIT`] is reported as hung and left behind.
    fn guarded(probe: &Probe, name: &str, check: impl FnOnce(Probe) + Send + 'static) {
        let (done_tx, done) = std::sync::mpsc::channel();
        let worker = probe.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("probe-{name}"))
            .spawn(move || {
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(worker)));
                let _told = done_tx.send(outcome.is_ok());
            });
        if let Err(err) = spawned {
            probe.report.fail(name, format!("could not start: {err}"));
            return;
        }
        match done.recv_timeout(CHECK_LIMIT) {
            Ok(true) => {}
            Ok(false) => probe.report.fail(name, "panicked (see the message above)"),
            Err(_) => probe.report.fail(
                name,
                format!("did not finish within {} s (hung)", CHECK_LIMIT.as_secs()),
            ),
        }
    }

    /// A PowerShell WinForms window titled `title`, recoloured every
    /// 30 ms, closing itself after `seconds`.
    fn spawn_window(title: &str, seconds: u32, left: i32) -> Option<Child> {
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; \
             $f = New-Object System.Windows.Forms.Form; $f.Text = '{title}'; $f.Width = 360; $f.Height = 240; \
             $f.StartPosition = 'Manual'; $f.Left = {left}; $f.Top = 60; $f.TopMost = $true; \
             $script:i = 0; $t = New-Object System.Windows.Forms.Timer; $t.Interval = 30; \
             $t.Add_Tick({{ $script:i++; $f.BackColor = [System.Drawing.Color]::FromArgb(($script:i*7)%256, ($script:i*13)%256, ($script:i*29)%256); \
             if ($script:i -gt ({seconds}*33)) {{ $f.Close() }} }}); $t.Start(); \
             [System.Windows.Forms.Application]::Run($f)"
        );
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &script,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()
    }

    fn find_window(probe: &Probe, backend: BackendId, title: &str) -> Option<Source> {
        let list = block(probe.client.sources(SourceFilter {
            kinds: vec![SourceKind::Window],
            backend: Some(backend),
        }))
        .ok()?;
        list.sources.into_iter().find(|s| s.name == title)
    }

    fn events_of(session: &SessionHandle) -> Vec<CaptureEvent> {
        let Ok(stream) = session.events() else {
            return Vec::new();
        };
        let mut events = Vec::new();
        while let Some(Ok(StreamItem::Event(event))) = stream.try_recv() {
            events.push(event);
        }
        events
    }

    fn describe_caps(caps: &BackendCaps) -> String {
        let formats: Vec<String> = caps
            .video
            .iter()
            .map(|f| format!("{:?}/{:?}{:?}", f.format, f.memory, f.copies))
            .collect();
        format!(
            "kinds {:?}; formats [{}]; cursor {:?}; damage {:?}; orientation {:?}; hdr {}; producer {:?}; \
             persistence {:?}; pool {}..{} (default {}) {:?}; copies {:?}; protected {:?}; window {:?}; \
             concurrency {:?}; provenance {:?}; notes {:?}",
            caps.source_kinds,
            formats.join(", "),
            caps.cursor,
            caps.damage,
            caps.orientation,
            caps.colour.hdr,
            caps.producer,
            caps.persistence,
            caps.pool.min_depth,
            caps.pool.max_depth,
            caps.pool.default_depth,
            caps.pool.exhaustion,
            caps.copies,
            caps.protected_content,
            caps.window,
            caps.concurrency,
            caps.provenance,
            caps.notes,
        )
    }

    /// Frames of `session` for `duration`, every one checked.
    struct Stats {
        frames: u32,
        seq_ok: bool,
        wall_ok: bool,
        pts_ok: bool,
        repeats: u32,
        damage_rects: u32,
        damage_full: u32,
        textures: u32,
        shared: u32,
        cursor: u32,
        memory: Option<BufferMemory>,
        protected: u32,
        first_error: Option<String>,
    }

    fn consume(session: &SessionHandle, duration: Duration) -> Stats {
        let mut stats = Stats {
            frames: 0,
            seq_ok: true,
            wall_ok: true,
            pts_ok: true,
            repeats: 0,
            damage_rects: 0,
            damage_full: 0,
            textures: 0,
            shared: 0,
            cursor: 0,
            memory: None,
            protected: 0,
            first_error: None,
        };
        let mut last: Option<FrameMeta> = None;
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            let Some(frame) = session.video().try_recv() else {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            };
            check_frame(&frame, last.as_ref(), &mut stats);
            last = Some(frame.meta.clone());
        }
        stats
    }

    fn check_frame(frame: &NativeFrame<FrameMeta>, last: Option<&FrameMeta>, stats: &mut Stats) {
        let meta = &frame.meta;
        stats.frames += 1;
        stats.memory = Some(meta.memory);
        if let Some(last) = last {
            if meta.seq <= last.seq {
                stats.seq_ok = false;
            }
            if meta.pts_ns < last.pts_ns && !meta.repeat {
                stats.pts_ok = false;
            }
        }
        if now_us().abs_diff(meta.wall_us) > 1_000_000 {
            stats.wall_ok = false;
            if stats.first_error.is_none() {
                stats.first_error = Some(format!("wall_us {} vs now {}", meta.wall_us, now_us()));
            }
        }
        if meta.repeat {
            stats.repeats += 1;
        }
        match &meta.damage {
            istmo_screen_capture::Damage::Rects(_) => stats.damage_rects += 1,
            istmo_screen_capture::Damage::Full => stats.damage_full += 1,
            istmo_screen_capture::Damage::Unchanged => {}
        }
        if meta.cursor.is_some() {
            stats.cursor += 1;
        }
        if meta.protected_content {
            stats.protected += 1;
        }
        if meta.memory == BufferMemory::D3d11Texture {
            match frame.buffer.downcast_ref::<D3d11Frame>() {
                Some(texture) => {
                    stats.textures += 1;
                    if texture.shared_handle().is_some() {
                        stats.shared += 1;
                    }
                }
                None => {
                    if stats.first_error.is_none() {
                        stats.first_error =
                            Some("a D3d11Texture frame without a D3d11Frame backing".to_owned());
                    }
                }
            }
        } else if meta.memory == BufferMemory::Cpu {
            let stride = meta.planes.first().map_or(0, |p| p.stride);
            let needed = u64::from(stride) * u64::from(meta.size.height);
            let have = frame.buffer.as_bytes().map_or(0, <[u8]>::len) as u64;
            if have < needed && stats.first_error.is_none() {
                stats.first_error = Some(format!("CPU frame of {have} bytes, {needed} needed"));
            }
        }
    }

    fn open(
        probe: &Probe,
        config: SessionConfig,
        policy: SinkPolicy,
    ) -> Result<SessionHandle, CaptureError> {
        block(probe.client.open_session(&probe.runtime, config, policy))
    }

    /// A checksum of a texture's pixels, read back through a staging
    /// texture on the frame's own device.
    #[allow(unsafe_code, reason = "a D3D11 staging readback")]
    fn texture_sum(frame: &D3d11Frame) -> Option<u64> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_STAGING,
        };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: a live texture; `desc` is a writable out value.
        unsafe { frame.texture().GetDesc(&raw mut desc) };
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.MiscFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0.cast_unsigned();
        let mut staging = None;
        // SAFETY: `desc` and the out pointer are live locals.
        unsafe {
            frame
                .device()
                .CreateTexture2D(&raw const desc, None, Some(&raw mut staging))
        }
        .ok()?;
        let staging = staging?;
        // SAFETY: the device is live.
        let context = unsafe { frame.device().GetImmediateContext() }.ok()?;
        // SAFETY: same size and format, same device.
        unsafe { context.CopyResource(&staging, frame.texture()) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: a CPU-readable staging texture; `mapped` is live.
        unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped)) }.ok()?;
        let len = usize::try_from(mapped.RowPitch).ok()? * usize::try_from(desc.Height).ok()?;
        let sum = (!mapped.pData.is_null()).then(|| {
            // SAFETY: the mapping holds `Height` rows of `RowPitch` bytes
            // until Unmap below.
            let bytes = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), len) };
            bytes.iter().map(|b| u64::from(*b)).sum()
        });
        // SAFETY: unmaps what was mapped above; `bytes` is gone.
        unsafe { context.Unmap(&staging, 0) };
        sum
    }

    /// WGC discards its frames on `Close`: does a zero-copy frame the
    /// consumer still holds keep its pixels once the session closed?
    fn held_across_close(probe: &Probe) {
        let report = &probe.report;
        let check = "windows-wgc frame held across close";
        let config = SessionConfig::default()
            .backend(fixed(BackendId::WindowsWgc))
            .fps(FrameRate::Native);
        let session = match open(probe, config, SinkPolicy::latest()) {
            Ok(session) => session,
            Err(err) => return report.fail(check, err),
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        let frame = loop {
            if let Some(frame) = session.video().try_recv() {
                break Some(frame);
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let Some(frame) = frame else {
            return report.skip(check, "no frame in 3 s");
        };
        let Some(texture) = frame.buffer.downcast_ref::<D3d11Frame>() else {
            return report.skip(check, "not a D3D11 frame");
        };
        let before = texture_sum(texture);
        let closed_at = Instant::now();
        let closed = block(session.close());
        let waited = closed_at.elapsed();
        let after = texture_sum(texture);
        let detail = format!(
            "close {:?} after {} ms (the plugin waits up to 500 ms for held frames), checksum before {before:?} after {after:?}",
            closed.err(),
            waited.as_millis()
        );
        if before.is_some() && before == after {
            report.pass(check, detail);
        } else {
            report.warn(check, detail);
        }
    }

    fn non_black(bytes: &[u8], stride: u32, width: u32, height: u32) -> f64 {
        let mut lit = 0_u32;
        let mut seen = 0_u32;
        let mut y = 0;
        while y < height {
            let mut x = 0;
            while x < width {
                let at = (y * stride + x * 4) as usize;
                if let Some(px) = bytes.get(at..at + 3) {
                    seen += 1;
                    if px.iter().any(|c| *c > 8) {
                        lit += 1;
                    }
                }
                x += 37;
            }
            y += 41;
        }
        if seen == 0 {
            0.0
        } else {
            f64::from(lit) / f64::from(seen)
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the per-backend checklist, step by step"
    )]
    fn backend_section(probe: &Probe, backend: BackendId, animator: Option<&Source>) {
        let report = &probe.report;
        let name = backend.as_str();
        report.section(&format!("backend {name}"));
        let caps = match block(probe.client.capabilities(CapabilityQuery {
            backend: Some(backend),
            ..CapabilityQuery::default()
        })) {
            Ok(report) => report.backends.into_iter().next(),
            Err(err) => {
                report.fail(&format!("{name} capabilities"), err);
                return;
            }
        };
        let Some(caps) = caps else {
            report.fail(&format!("{name} capabilities"), "no report");
            return;
        };
        report.pass(&format!("{name} capabilities"), describe_caps(&caps));

        // Sources.
        match block(probe.client.sources(SourceFilter {
            kinds: Vec::new(),
            backend: Some(backend),
        })) {
            Ok(list) => {
                let monitors: Vec<&Source> = list
                    .sources
                    .iter()
                    .filter(|s| s.kind == SourceKind::Monitor)
                    .collect();
                let windows: Vec<&Source> = list
                    .sources
                    .iter()
                    .filter(|s| s.kind == SourceKind::Window)
                    .collect();
                report.check(
                    !monitors.is_empty(),
                    &format!("{name} sources"),
                    format!("{} monitors, {} windows", monitors.len(), windows.len()),
                );
                for m in &monitors {
                    report.info(
                        &format!("{name} monitor"),
                        format!(
                            "id {} | name {:?} make {:?} model {:?} connector {:?} desc {:?} | geometry {:?} refresh {:?} mHz | orientation {:?} | primary {}",
                            m.id.value, m.name, m.make, m.model, m.connector, m.description, m.geometry, m.refresh_mhz, m.orientation, m.flags.primary
                        ),
                    );
                }
                let apps: Vec<String> = windows
                    .iter()
                    .take(8)
                    .map(|w| {
                        format!(
                            "{}(pid {:?}{}{})",
                            w.app_id.as_deref().unwrap_or("?"),
                            w.pid,
                            if w.flags.minimised { ", minimised" } else { "" },
                            if w.flags.protected { ", protected" } else { "" }
                        )
                    })
                    .collect();
                if !windows.is_empty() {
                    report.info(
                        &format!("{name} windows"),
                        format!("first apps (titles not shown): {}", apps.join(", ")),
                    );
                }
            }
            Err(err) => report.fail(&format!("{name} sources"), err),
        }

        // CPU screenshot of the primary monitor.
        let started = Instant::now();
        match block(probe.client.screenshot_cpu(
            ScreenshotRequest::default().backend(fixed(backend)),
            PixelLayout::default(),
        )) {
            Ok(image) => {
                let lit = non_black(&image.bytes, image.stride, image.width, image.height);
                let detail = format!(
                    "{}x{} stride {} {:?}, {:.0}% non-black samples, {} ms",
                    image.width,
                    image.height,
                    image.stride,
                    image.format,
                    lit * 100.0,
                    started.elapsed().as_millis()
                );
                if image.width == 0 || image.height == 0 {
                    report.fail(&format!("{name} screenshot_cpu"), detail);
                } else if lit < 0.01 {
                    report.warn(
                        &format!("{name} screenshot_cpu"),
                        format!("{detail} (all black?)"),
                    );
                } else {
                    report.pass(&format!("{name} screenshot_cpu"), detail);
                }
            }
            Err(err) => report.fail(&format!("{name} screenshot_cpu"), err),
        }

        // All monitors, one composite.
        match block(probe.client.screenshot_cpu(
            ScreenshotRequest::of(SourceSelector::AllMonitors).backend(fixed(backend)),
            PixelLayout::default(),
        )) {
            Ok(image) => report.pass(
                &format!("{name} screenshot all monitors"),
                format!("{}x{}", image.width, image.height),
            ),
            Err(err) => report.fail(&format!("{name} screenshot all monitors"), err),
        }

        let gpu = caps
            .video
            .iter()
            .any(|f| f.memory == BufferMemory::D3d11Texture);
        // A GPU screenshot, as frames.
        if gpu {
            let request = ScreenshotRequest::default()
                .backend(fixed(backend))
                .formats(FormatRequest {
                    tiers: vec![
                        texture_tier(PixelFormat::Bgra8)
                            .allowing(CopyStage::DxgiMandatory)
                            .allowing(CopyStage::LiveBufferSnapshot),
                    ],
                });
            match block(probe.client.screenshot_frames(&probe.runtime, request)) {
                Ok((info, frames)) => {
                    let frame = frames.first();
                    let backing = frame.and_then(|f| f.buffer.downcast_ref::<D3d11Frame>());
                    report.check(
                        backing.is_some(),
                        &format!("{name} screenshot as a D3D11 texture"),
                        format!(
                            "{} frame(s), memory {:?}, copies {:?}, shared handle {}",
                            info.frames,
                            frame.map(|f| f.meta.memory),
                            info.negotiated.copies,
                            backing.is_some_and(|b| b.shared_handle().is_some())
                        ),
                    );
                }
                Err(err) => report.fail(&format!("{name} screenshot as a D3D11 texture"), err),
            }
        }

        if backend == BackendId::WindowsWgc {
            held_across_close(probe);
        }

        // A 5 s stream with the backend's own (copy-free where it can) format.
        {
            let check = format!("{name} 5 s stream");
            let config = SessionConfig::default()
                .backend(fixed(backend))
                .fps(FrameRate::Native)
                .pool_depth(4);
            match open(probe, config, SinkPolicy::latest()) {
                Ok(session) => {
                    let n = session.negotiated().clone();
                    report.info(
                        &format!("{name} negotiated"),
                        format!(
                            "{}x{} {:?} {:?} tier {} copies {:?} pool {} producer {:?} sync {:?} damage {:?} cursor {:?} border {:?} indicator {:?} orientation {:?}",
                            n.size.width, n.size.height, n.format, n.memory, n.tier, n.copies, n.pool_depth, n.producer, n.sync, n.damage, n.cursor, n.border, n.indicator, n.source.orientation
                        ),
                    );
                    let stats = consume(&session, Duration::from_secs(5));
                    let events = events_of(&session);
                    let started = matches!(events.first(), Some(CaptureEvent::Started(_)));
                    let fps = f64::from(stats.frames) / 5.0;
                    let ok = stats.frames >= 5
                        && stats.seq_ok
                        && stats.wall_ok
                        && stats.pts_ok
                        && started
                        && stats.first_error.is_none();
                    report.check(
                        ok,
                        &check,
                        format!(
                            "{} frames ({fps:.1} fps), seq ok {}, wall_us ok {}, pts ok {}, Started first {}, damage rects/full {}/{}, D3D11 frames {} (shared {}), cursor meta {}, protected {}{}",
                            stats.frames, stats.seq_ok, stats.wall_ok, stats.pts_ok, started, stats.damage_rects, stats.damage_full, stats.textures, stats.shared, stats.cursor, stats.protected,
                            stats.first_error.map(|e| format!(", error: {e}")).unwrap_or_default()
                        ),
                    );
                    let closed_at = Instant::now();
                    let closed = block(session.close());
                    report.check(
                        closed.is_ok() && closed_at.elapsed() < Duration::from_secs(3),
                        &format!("{name} close"),
                        format!(
                            "{:?} in {} ms",
                            closed.err(),
                            closed_at.elapsed().as_millis()
                        ),
                    );
                }
                Err(err) => report.fail(&check, err),
            }
        }

        // The CPU path of the GPU backends.
        if gpu {
            let check = format!("{name} CPU stream");
            let config = SessionConfig::default()
                .backend(fixed(backend))
                .fps(FrameRate::Max(30))
                .cpu_frames();
            match open(probe, config, SinkPolicy::latest()) {
                Ok(session) => {
                    let copies = session.negotiated().copies.clone();
                    let stats = consume(&session, Duration::from_secs(2));
                    report.check(
                        stats.frames > 0
                            && stats.memory == Some(BufferMemory::Cpu)
                            && copies.contains(&CopyStage::GpuReadback)
                            && stats.first_error.is_none(),
                        &check,
                        format!(
                            "{} frames, memory {:?}, copies {copies:?}{}",
                            stats.frames,
                            stats.memory,
                            stats
                                .first_error
                                .map(|e| format!(", error: {e}"))
                                .unwrap_or_default()
                        ),
                    );
                }
                Err(err) => report.fail(&check, err),
            }
        }

        // WGC: zero-copy textures, then shareable copies for another device.
        if backend == BackendId::WindowsWgc {
            let check = format!("{name} zero-copy texture stream");
            let config = SessionConfig::default()
                .backend(fixed(backend))
                .fps(FrameRate::Native)
                .formats(FormatRequest {
                    tiers: vec![texture_tier(PixelFormat::Bgra8)],
                });
            match open(probe, config, SinkPolicy::latest()) {
                Ok(session) => {
                    let copies = session.negotiated().copies.clone();
                    let stats = consume(&session, Duration::from_secs(2));
                    report.check(
                        stats.textures > 0 && copies.is_empty() && stats.shared == 0,
                        &check,
                        format!(
                            "{} texture frames, copies {copies:?}, with handle {}",
                            stats.textures, stats.shared
                        ),
                    );
                }
                Err(err) => report.fail(&check, err),
            }
            let check = format!("{name} shareable (CrossDevice) stream");
            if let Some(GpuIdentity::Luid(luid)) = caps.producer {
                let config = SessionConfig::default()
                    .backend(fixed(backend))
                    .fps(FrameRate::Native)
                    .gpu(GpuPreference::Same(GpuIdentity::Luid(luid)))
                    .formats(FormatRequest {
                        tiers: vec![
                            texture_tier(PixelFormat::Bgra8).allowing(CopyStage::CrossDevice),
                        ],
                    });
                match open(probe, config, SinkPolicy::latest()) {
                    Ok(session) => {
                        let copies = session.negotiated().copies.clone();
                        let stats = consume(&session, Duration::from_secs(2));
                        report.check(
                            stats.shared > 0 && copies.contains(&CopyStage::CrossDevice),
                            &check,
                            format!(
                                "{} frames with an NT handle, copies {copies:?}",
                                stats.shared
                            ),
                        );
                    }
                    Err(err) => report.fail(&check, err),
                }
            } else {
                report.skip(&check, "no producer LUID in the report");
            }
            // HDR (FP16) where the caller asks for it.
            let config = SessionConfig::default()
                .backend(fixed(backend))
                .fps(FrameRate::Native)
                .formats(FormatRequest {
                    tiers: vec![texture_tier(PixelFormat::Rgba16f)],
                });
            let mut config = config;
            config.video.hdr = istmo_screen_capture::HdrPolicy::PreferHdr;
            match open(probe, config, SinkPolicy::latest()) {
                Ok(session) => {
                    let n = session.negotiated().clone();
                    let stats = consume(&session, Duration::from_secs(1));
                    report.check(
                        n.format == PixelFormat::Rgba16f,
                        &format!("{name} FP16 stream"),
                        format!("{:?} {:?}, {} frames", n.format, n.colour, stats.frames),
                    );
                }
                Err(err) => report.fail(&format!("{name} FP16 stream"), err),
            }
        }

        // DXGI: the pointer as metadata.
        if backend == BackendId::WindowsDxgi {
            let config = SessionConfig::default()
                .backend(fixed(backend))
                .fps(FrameRate::Native)
                .cursor(CursorRequest {
                    mode: CursorMode::Metadata,
                    strength: Strength::Require,
                });
            match open(probe, config, SinkPolicy::latest()) {
                Ok(session) => {
                    let stats = consume(&session, Duration::from_secs(2));
                    if stats.cursor > 0 {
                        report.pass(
                            &format!("{name} cursor metadata"),
                            format!(
                                "{} of {} frames carry the pointer",
                                stats.cursor, stats.frames
                            ),
                        );
                    } else {
                        report.warn(&format!("{name} cursor metadata"), format!("no frame carried the pointer in 2 s ({} frames); move the mouse during the run", stats.frames));
                    }
                }
                Err(err) => report.fail(&format!("{name} cursor metadata"), err),
            }
        }

        // Window capture.
        if caps.source_kinds.contains(&SourceKind::Window) {
            let check = format!("{name} window stream");
            match animator.and_then(|_| find_window(probe, backend, ANIMATOR_TITLE)) {
                Some(window) => {
                    let config = SessionConfig::of(SourceSelector::Id(window.id))
                        .backend(fixed(backend))
                        .fps(FrameRate::Native);
                    match open(probe, config, SinkPolicy::latest()) {
                        Ok(session) => {
                            let stats = consume(&session, Duration::from_secs(2));
                            report.check(
                                stats.frames > 0,
                                &check,
                                format!("{} frames of the probe's own window", stats.frames),
                            );
                        }
                        Err(err) => report.fail(&check, err),
                    }
                }
                None => report.skip(
                    &check,
                    "the probe's window is not listed (PowerShell unavailable?)",
                ),
            }
        }

        // Exhaustion, Stall: a consumer that never reads.
        {
            let check = format!("{name} exhaustion (Stall)");
            let config = SessionConfig::default()
                .backend(fixed(backend))
                .fps(FrameRate::Native)
                .pool_depth(3);
            match open(probe, config, SinkPolicy::queue(16)) {
                Ok(session) => {
                    let limit = u64::from(session.negotiated().pool_depth.saturating_sub(1));
                    std::thread::sleep(Duration::from_secs(2));
                    let accepted = session.video().stats().accepted;
                    let exhausted = events_of(&session)
                        .iter()
                        .filter(|e| matches!(e, CaptureEvent::Exhausted(_)))
                        .count();
                    drop(session.video().recv());
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while session.video().stats().accepted <= accepted && Instant::now() < deadline
                    {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    let resumed = session.video().stats().accepted > accepted;
                    let memory_cpu = session.negotiated().memory == BufferMemory::Cpu
                        && backend != BackendId::WindowsGdi;
                    if memory_cpu {
                        report.skip(&check, "CPU frames hold no pool buffer");
                    } else {
                        report.check(
                            accepted <= limit && exhausted == 1 && resumed,
                            &check,
                            format!("held {accepted} (limit {limit}), Exhausted events {exhausted}, resumed after a release {resumed}"),
                        );
                    }
                }
                Err(err) => report.fail(&check, err),
            }
        }

        // Exhaustion, CopyToCpu: frames keep coming while every buffer is held.
        {
            let check = format!("{name} exhaustion (CopyToCpu)");
            let config = SessionConfig::default()
                .backend(fixed(backend))
                .fps(FrameRate::Native)
                .pool_depth(3)
                .exhaustion(Exhaustion::CopyToCpu);
            match open(probe, config, SinkPolicy::queue(64)) {
                Ok(session) => {
                    let mut held = Vec::new();
                    let deadline = Instant::now() + Duration::from_secs(6);
                    while held.len() < 6 && Instant::now() < deadline {
                        if let Some(frame) = session.video().try_recv() {
                            held.push(frame);
                        } else {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                    }
                    let read_back = held
                        .iter()
                        .filter(|f| f.meta.copied == Some(CopyStage::ExhaustionReadback))
                        .count();
                    report.check(
                        held.len() == 6 && read_back > 0,
                        &check,
                        format!("{} frames held, {read_back} read back", held.len()),
                    );
                }
                Err(err) => report.fail(&check, err),
            }
        }

        // A sink dropped mid-stream: the session stops without hanging.
        {
            let config = SessionConfig::default()
                .backend(fixed(backend))
                .fps(FrameRate::Native);
            match open(probe, config, SinkPolicy::latest()) {
                Ok(session) => {
                    std::thread::sleep(Duration::from_millis(300));
                    let at = Instant::now();
                    drop(session);
                    report.check(
                        at.elapsed() < Duration::from_secs(3),
                        &format!("{name} sink dropped mid-stream"),
                        format!("dropped in {} ms", at.elapsed().as_millis()),
                    );
                }
                Err(err) => report.fail(&format!("{name} sink dropped mid-stream"), err),
            }
        }
    }

    fn watch_section(probe: &Probe) {
        let report = &probe.report;
        report.section("watch_sources");
        let watch = match block(probe.client.source_watch(SourceFilter {
            kinds: Vec::new(),
            backend: Some(BackendId::WindowsGdi),
        })) {
            Ok(watch) => watch,
            Err(err) => {
                report.fail("watch_sources", err);
                return;
            }
        };
        report.pass(
            "watch_sources listing",
            format!("{} sources listed first", watch.sources().len()),
        );
        let Some(mut child) = spawn_window(WATCH_TITLE, 8, 480) else {
            report.skip(
                "watch_sources window opened / closed",
                "PowerShell could not start",
            );
            return;
        };
        let mut watch = watch;
        let mut added = None;
        let mut removed = false;
        // Whether the window showed in a plain listing: without it, a
        // missing `Added` says nothing about the watch.
        let mut appeared = false;
        let mut last_look = Instant::now();
        let deadline = Instant::now() + Duration::from_secs(25);
        while Instant::now() < deadline && !removed {
            if !appeared && last_look.elapsed() > Duration::from_millis(500) {
                last_look = Instant::now();
                appeared = find_window(probe, BackendId::WindowsGdi, WATCH_TITLE).is_some();
            }
            match watch.try_recv() {
                Some(Ok(SourceChange::Added(source))) if source.name == WATCH_TITLE => {
                    added = Some(source.id);
                }
                Some(Ok(SourceChange::Removed(id))) if added.as_ref() == Some(&id) => {
                    removed = true;
                }
                Some(Err(err)) => {
                    report.fail("watch_sources", err);
                    break;
                }
                Some(Ok(_)) => {}
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        let _killed = child.kill();
        let _reaped = child.wait();
        if added.is_none() && !appeared {
            report.skip(
                "watch_sources window opened / closed",
                "the probe's PowerShell window never appeared in a listing either",
            );
            return;
        }
        report.check(
            added.is_some(),
            "watch_sources window opened",
            if added.is_some() {
                "Added arrived"
            } else {
                "no Added for the probe's window in 25 s"
            },
        );
        report.check(
            removed,
            "watch_sources window closed",
            if removed {
                "Removed arrived"
            } else {
                "no Removed for the probe's window"
            },
        );
    }

    fn audio_check(probe: &Probe, name: &str, config: AudioConfig, seconds: u64) {
        let report = &probe.report;
        let session = match block(probe.client.open_audio_session(
            &probe.runtime,
            config,
            SinkPolicy::queue(1024).with_overflow(Overflow::DropNewest),
        )) {
            Ok(session) => session,
            Err(CaptureError::OsTooOld(version)) => {
                report.skip(
                    name,
                    format!(
                        "needs build 20348+ (this is {}.{}.{})",
                        version.major, version.minor, version.patch
                    ),
                );
                return;
            }
            Err(err) => {
                report.fail(name, err);
                return;
            }
        };
        let negotiated = session.negotiated().clone();
        let started = Instant::now();
        let mut chunks = Vec::new();
        while started.elapsed() < Duration::from_secs(seconds) {
            match session.audio().try_recv() {
                Some(chunk) => chunks.push(chunk.meta),
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        let frames: u64 = chunks.iter().map(|c| u64::from(c.frames)).sum();
        let expected = u64::from(negotiated.rate) * seconds;
        let contiguous = chunks.windows(2).all(|p| p[1].seq == p[0].seq + 1);
        let wall = chunks
            .last()
            .is_some_and(|c| now_us().abs_diff(c.wall_us) < 500_000);
        let discontinuities = chunks.iter().filter(|c| c.discontinuity).count();
        let ok = !chunks.is_empty()
            && contiguous
            && wall
            && frames * 10 >= expected * 7
            && frames * 10 <= expected * 13;
        report.check(
            ok,
            name,
            format!(
                "{:?}: {} Hz {} ch {:?}; {} chunks, {frames} frames (expected ~{expected}), contiguous {contiguous}, wall_us near now {wall}, discontinuities {discontinuities}",
                negotiated.source, negotiated.rate, negotiated.channels, negotiated.format, chunks.len()
            ),
        );
        let _closed = block(session.close());
    }

    fn audio_section(probe: &Probe) {
        let report = &probe.report;
        report.section("audio");
        match block(probe.client.capabilities(CapabilityQuery::default())) {
            Ok(caps) => match caps.audio {
                Some(audio) => report.check(
                    audio.availability == Availability::Ready,
                    "audio backend",
                    format!(
                        "{:?}, sources {:?}, formats {:?}",
                        audio.availability, audio.caps.sources, audio.caps.formats
                    ),
                ),
                None => report.fail("audio backend", "none compiled"),
            },
            Err(err) => report.fail("audio backend", err),
        }
        audio_check(
            probe,
            "audio System loopback (48 kHz F32 stereo)",
            AudioConfig::of(AudioSource::System)
                .rate(48_000)
                .channels(2)
                .format(SampleFormat::F32),
            3,
        );
        audio_check(
            probe,
            "audio System loopback (44.1 kHz S16)",
            AudioConfig::of(AudioSource::System)
                .rate(44_100)
                .format(SampleFormat::S16),
            2,
        );
        audio_check(
            probe,
            "audio ExcludeSelf (process loopback)",
            AudioConfig::of(AudioSource::ExcludeSelf)
                .rate(48_000)
                .channels(2),
            2,
        );
        audio_check(
            probe,
            "audio Application(explorer) (process loopback)",
            AudioConfig::of(AudioSource::Application("explorer".to_owned()))
                .rate(48_000)
                .channels(2),
            2,
        );

        // Video and audio in one session.
        let mut config = SessionConfig::default()
            .backend(fixed(BackendId::WindowsGdi))
            .fps(FrameRate::Max(15));
        config.audio = Some(
            AudioConfig::of(AudioSource::System)
                .rate(48_000)
                .channels(2),
        );
        match block(probe.client.open_session_with_audio(
            &probe.runtime,
            config,
            SinkPolicy::latest(),
        )) {
            Ok(session) => {
                let started = Instant::now();
                let (mut video, mut audio) = (0, 0);
                while started.elapsed() < Duration::from_secs(3) {
                    if session.video().try_recv().is_some() {
                        video += 1;
                    }
                    if let Some(sink) = session.audio() {
                        while sink.try_recv().is_some() {
                            audio += 1;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                report.check(
                    video > 0 && audio > 0,
                    "video session with audio",
                    format!("{video} video frames, {audio} audio chunks in 3 s"),
                );
            }
            Err(err) => report.fail("video session with audio", err),
        }
    }

    fn security_section(probe: &Probe) {
        let report = &probe.report;
        report.section("access and origin");
        match block(probe.client.access(AccessScope::BorderlessCapture)) {
            Ok(state) => report.info(
                "borderless grant (not prompted)",
                format!(
                    "{:?}, model {:?}, persistence {:?}",
                    state.status, state.model, state.persistence
                ),
            ),
            Err(err) => report.fail("borderless grant", err),
        }
        let host = ScreenCaptureHost::new(DesktopCapture::default());
        let refused = codec::encode(&(SourceFilter::default(),))
            .ok()
            .and_then(|payload| {
                block(host.dispatch_from(
                    CallOrigin::Remote,
                    None,
                    "sources",
                    &payload,
                    CancelToken::new(),
                ))
                .ok()
            });
        let ok = matches!(&refused, Some(Outcome::DomainError(bytes))
            if codec::decode::<CaptureError>(bytes).is_ok_and(|(e, _)| e == CaptureError::OriginRefused));
        report.check(
            ok,
            "a remote caller is refused",
            "sources from CallOrigin::Remote",
        );
    }

    pub(crate) fn run() -> i32 {
        let report = Arc::new(Report::default());
        report.line(&format!(
            "istmo screen-capture Windows probe {} ({})",
            env!("CARGO_PKG_VERSION"),
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        ));
        // The probe ships no manifest: a DPI-unaware host, the case
        // `DuplicateOutput1` is said to refuse.
        report.info("process DPI awareness", dpi_awareness());
        let RuntimeInit { runtime, outbound } = match Runtime::init(RuntimeConfig::inline()) {
            Ok(init) => init,
            Err(err) => {
                report.fail("runtime", err);
                return 1;
            }
        };
        let _drain = std::thread::Builder::new()
            .name("outbound-drain".to_owned())
            .spawn(move || while outbound.recv().is_ok() {});
        runtime.register_host(ScreenCaptureHost::new(DesktopCapture::default()));
        let client = match ScreenCaptureClient::from_runtime(&runtime) {
            Ok(client) => Arc::new(client),
            Err(err) => {
                report.fail("client", err);
                return 1;
            }
        };
        let probe = Probe {
            runtime,
            client,
            report: Arc::clone(&report),
        };

        report.section("backends");
        let statuses = match block(probe.client.backends()) {
            Ok(statuses) => statuses,
            Err(err) => {
                report.fail("backends", err);
                Vec::new()
            }
        };
        let mut ready = Vec::new();
        let mut line = String::new();
        for status in &statuses {
            let _written = write!(
                line,
                "{} {:?}; ",
                status.backend.as_str(),
                status.availability
            );
            if status.availability == Availability::Ready {
                ready.push(status.backend);
            }
        }
        report.info("backends", line);
        if let Some(os) = statuses.iter().find_map(|s| match &s.availability {
            Availability::OsTooOld(v) => Some(*v),
            _ => None,
        }) {
            report.warn(
                "os",
                format!(
                    "a backend needs a newer Windows than {}.{}.{}",
                    os.major, os.minor, os.patch
                ),
            );
        }

        let animator_child = spawn_window(ANIMATOR_TITLE, 600, 40);
        if animator_child.is_none() {
            report.warn("animator", "PowerShell could not start: damage-driven backends may deliver few frames; move the mouse or play a video");
        }
        std::thread::sleep(Duration::from_secs(3));
        let animator = animator_child
            .as_ref()
            .and_then(|_| find_window(&probe, BackendId::WindowsGdi, ANIMATOR_TITLE));

        for backend in BACKENDS {
            if !ready.contains(&backend) {
                report.section(&format!("backend {}", backend.as_str()));
                report.skip(backend.as_str(), "not ready here (see backends)");
                continue;
            }
            let animator = animator.clone();
            guarded(&probe, backend.as_str(), move |probe| {
                backend_section(&probe, backend, animator.as_ref());
            });
        }
        guarded(&probe, "watch", |probe| watch_section(&probe));
        guarded(&probe, "audio", |probe| audio_section(&probe));
        guarded(&probe, "security", |probe| security_section(&probe));

        if let Some(mut child) = animator_child {
            let _killed = child.kill();
            let _reaped = child.wait();
        }
        report.section("summary");
        report.line(&report.summary());
        report.save("win_probe_report.txt");
        i32::from(report.failures() > 0)
    }
}

#[cfg(windows)]
fn main() {
    std::process::exit(probe::run());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("win_probe runs on Windows: build it with --target x86_64-pc-windows-msvc");
}
