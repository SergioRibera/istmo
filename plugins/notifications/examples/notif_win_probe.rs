//! Windows checklist of the notifications backend, for a real desktop.
//!
//! `notif_win_probe` runs the interactive checks and writes
//! `notif_win_probe_report.txt` next to the exe; `notif_win_probe cold`
//! schedules a toast and exits (click its button: Windows starts the
//! probe again through the COM activator); `notif_win_probe clean`
//! removes the registration.

#[cfg(not(windows))]
fn main() {
    eprintln!("notif_win_probe runs on Windows only");
}

#[cfg(windows)]
fn main() {
    probe::main();
}

#[cfg(windows)]
mod probe {
    use std::fmt::Write as _;
    use std::io::Write as _;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use istmo_core::Runtime;
    use istmo_notifications::desktop::windows::{WindowsApp, app_args};
    use istmo_notifications::{
        Action, DesktopNotifications, Notification, NotificationResponse, NotificationResponses,
        NotificationsClient, NotificationsHost, Progress, Trigger,
    };

    const AUMID: &str = "dev.istmo.notifprobe";

    struct Report {
        text: String,
        pass: u32,
        fail: u32,
    }

    impl Report {
        fn line(&mut self, status: &str, what: &str, detail: &str) {
            let line = format!("{status:<5} {what}: {detail}");
            println!("{line}");
            let _ = writeln!(self.text, "{line}");
            match status {
                "PASS" => self.pass += 1,
                "FAIL" => self.fail += 1,
                _ => {}
            }
        }

        fn check<T: std::fmt::Debug, E: std::fmt::Debug>(
            &mut self,
            what: &str,
            result: &Result<T, E>,
        ) -> bool {
            match result {
                Ok(value) => self.line("PASS", what, &format!("{value:?}")),
                Err(err) => self.line("FAIL", what, &format!("{err:?}")),
            }
            result.is_ok()
        }

        fn save(&self) {
            let path = std::env::current_exe()
                .ok()
                .and_then(|exe| {
                    exe.parent()
                        .map(|dir| dir.join("notif_win_probe_report.txt"))
                })
                .unwrap_or_else(|| "notif_win_probe_report.txt".into());
            let written = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut file| file.write_all(self.text.as_bytes()));
            match written {
                Ok(()) => println!("report: {}", path.display()),
                Err(err) => println!("report not written: {err}"),
            }
        }
    }

    fn ask(prompt: &str) {
        println!("\n>>> {prompt}");
    }

    fn wait_response(
        stream: &istmo_notifications::NotificationResponseStream,
        secs: u64,
    ) -> Option<NotificationResponse> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if let Some(Ok(response)) = stream.try_recv() {
                return Some(response);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }

    fn n(id: &str, title: &str) -> Notification {
        Notification::new(id, title).expect("valid id")
    }

    #[expect(clippy::too_many_lines, reason = "the checklist, top to bottom")]
    pub(super) fn main() {
        let mode = app_args()
            .first()
            .and_then(|arg| arg.to_str().map(str::to_owned));
        let app =
            WindowsApp::new(AUMID, "istmo notifications probe").with_icon_background(0xFF2B_6CB0);
        let mut report = Report {
            text: String::new(),
            pass: 0,
            fail: 0,
        };
        let _ = writeln!(
            report.text,
            "== notif_win_probe {} args={:?}",
            env!("CARGO_PKG_VERSION"),
            std::env::args().collect::<Vec<_>>()
        );
        if mode.as_deref() == Some("clean") {
            let removed = DesktopNotifications::unregister(&app);
            report.check("unregister", &removed);
            report.save();
            return;
        }
        let installed = DesktopNotifications::install(&app);
        if !report.check("install", &installed) {
            report.save();
            return;
        }
        let host = DesktopNotifications::default();
        let rt: Arc<Runtime> = Runtime::mock()
            .host(NotificationsHost::new(host.clone()))
            .runtime;
        host.attach(&rt);
        let client = NotificationsClient::from_runtime(&rt).expect("client");
        let responses = NotificationResponses::from_runtime(&rt)
            .expect("responses")
            .stream();

        if installed.as_ref().is_ok_and(|i| i.launched_by_notification) {
            report.line("INFO", "cold start", "started by the COM activator");
            match wait_response(&responses, 30) {
                Some(r) if r.launched_app => {
                    report.line("PASS", "cold activation", &format!("{r:?}"));
                }
                Some(r) => report.line(
                    "FAIL",
                    "cold activation (launched_app false)",
                    &format!("{r:?}"),
                ),
                None => report.line("FAIL", "cold activation", "no response within 30 s"),
            }
            report.save();
            return;
        }

        report.check("capabilities", &pollster::block_on(client.capabilities()));
        report.check("permission", &pollster::block_on(client.permission()));

        if mode.as_deref() == Some("cold") {
            let toast = n("probe.cold", "Cold start test")
                .with_body("Click Open now (the probe is closed)")
                .with_action(Action::button("open", "Open").expect("id"));
            let scheduled = pollster::block_on(
                client.schedule_checked(toast, Trigger::after(Duration::from_secs(15))),
            );
            report.check("schedule cold toast (15 s)", &scheduled);
            ask(
                "Close nothing: the probe exits now. In 15 s a toast appears: click 'Open'. The probe starts again and appends to the report.",
            );
            report.save();
            return;
        }

        let shown = pollster::block_on(
            client.show_checked(n("probe.plain", "Plain toast").with_body("Body line")),
        );
        report.check("show plain", &shown);

        let progress = n("probe.progress", "Downloading report.pdf")
            .with_status_text("Downloading...")
            .with_progress(Progress::fraction(0.0).with_label("report.pdf"));
        if let Ok(live) = pollster::block_on(client.show_live(progress))
            .inspect_err(|err| report.line("FAIL", "progress show", &format!("{err:?}")))
        {
            for step in 1..=20u8 {
                live.progress(f32::from(step) / 20.0);
                std::thread::sleep(Duration::from_millis(150));
            }
            let finished = pollster::block_on(
                live.finish(n("probe.progress", "Download done").with_body("report.pdf")),
            );
            report.check("progress updates + finish", &finished);
            ask(
                "Did the bar fill in place WITHOUT popping up again for every step? Note it in the report (y/n).",
            );
        }

        let buttons = n("probe.reply", "Reply test")
            .with_body("Type something and press Send, or pick a choice")
            .with_user_data("user-data-42")
            .with_url("istmo-probe://reply")
            .with_action(Action::reply("reply", "Send").expect("id"))
            .with_action(Action::select("mood", "Mood", ["Good", "Bad"]).expect("id"))
            .with_action(Action::button("later", "Later").expect("id").background());
        report.check(
            "show reply + selection + background button",
            &pollster::block_on(client.show_checked(buttons)),
        );
        ask("In the toast: type a reply and press Send (60 s).");
        match wait_response(&responses, 60) {
            Some(r) => report.line(
                if r.reply.is_some() { "PASS" } else { "WARN" },
                "reply response",
                &format!("{r:?}"),
            ),
            None => report.line("FAIL", "reply response", "none within 60 s"),
        }

        let delivered = pollster::block_on(client.delivered());
        let listed = delivered
            .as_ref()
            .map(|list| list.iter().map(|d| d.id.value.clone()).collect::<Vec<_>>());
        report.check("delivered (history)", &listed);

        let scheduled = pollster::block_on(client.schedule_checked(
            n("probe.scheduled", "Scheduled toast").with_body("Due 20 s after scheduling"),
            Trigger::after(Duration::from_secs(20)),
        ));
        report.check("schedule (20 s)", &scheduled);
        report.check("pending", &pollster::block_on(client.pending()));
        ask("Wait ~25 s: exactly ONE 'Scheduled toast' should pop up (not two).");
        std::thread::sleep(Duration::from_secs(25));

        report.check(
            "cancel plain",
            &pollster::block_on(client.cancel(n("probe.plain", "x").id)),
        );
        let after = pollster::block_on(client.delivered())
            .map(|list| list.iter().any(|d| d.id.value == "probe.plain"));
        match after {
            Ok(false) => report.line("PASS", "cancel removed it from the history", ""),
            other => report.line(
                "FAIL",
                "cancel removed it from the history",
                &format!("{other:?}"),
            ),
        }
        let repeat = pollster::block_on(client.schedule_checked(
            n("probe.repeat", "Repeating toast").with_body("Every 20 s"),
            Trigger::every(Duration::from_secs(20)),
        ));
        report.check("schedule repeat (every 20 s)", &repeat);
        ask(
            "Wait ~45 s: 'Repeating toast' should pop up TWICE, once per occurrence (not four times).",
        );
        std::thread::sleep(Duration::from_secs(45));
        report.check(
            "cancel repeat",
            &pollster::block_on(client.cancel(n("probe.repeat", "x").id)),
        );

        for id in ["probe.all.1", "probe.all.2"] {
            report.check(
                "show for cancel_all",
                &pollster::block_on(client.show_checked(n(id, "Cancel all test"))),
            );
        }
        report.check("cancel_all", &pollster::block_on(client.cancel_all()));
        match pollster::block_on(client.delivered()) {
            Ok(list) if list.is_empty() => {
                report.line("PASS", "cancel_all emptied the history", "");
            }
            other => report.line(
                "FAIL",
                "cancel_all emptied the history",
                &format!("{other:?}"),
            ),
        }

        match DesktopNotifications::revoke_activator() {
            Ok(true) => report.line("PASS", "revoke activator", ""),
            other => report.line("FAIL", "revoke activator", &format!("{other:?}")),
        }

        report.check(
            "open settings",
            &pollster::block_on(client.open_settings(None)),
        );

        let _ = writeln!(report.text, "== PASS {} FAIL {}", report.pass, report.fail);
        println!("\nPASS {}  FAIL {}", report.pass, report.fail);
        report.save();
    }
}
