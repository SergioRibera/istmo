//! Login starts under systemd, read from `/proc/self/cgroup`.
//!
//! GNOME 3.34+ and Plasma 5.20+ run XDG autostart entries through
//! `systemd-xdg-autostart-generator`, as `app-<id>@autostart.service`
//! units of the user manager. A process started that way sits in a
//! cgroup ending in `@autostart.service`.
//!
//! The signal only exists where the user manager runs the session's
//! apps: their cgroups are below `user@<uid>.service`. Elsewhere (a
//! session scope of `logind` without a systemd-managed desktop, no
//! systemd at all) an autostart entry starts the app directly and the
//! cgroup says nothing either way.

use crate::desktop::Signal;

/// Unit suffix of the generator's autostart services.
const AUTOSTART_UNIT_SUFFIX: &str = "@autostart.service";

/// Read the signal out of `/proc/self/cgroup` text (cgroup v1 or v2).
pub(crate) fn signal(text: &str) -> Signal {
    let mut user_manager = false;
    for line in text.lines() {
        // `hierarchy-id:controllers:path`; the path may contain `:`.
        let Some(path) = line.splitn(3, ':').nth(2) else {
            continue;
        };
        if path
            .rsplit('/')
            .next()
            .is_some_and(|unit| unit.ends_with(AUTOSTART_UNIT_SUFFIX))
        {
            return Signal::Login;
        }
        user_manager |= path
            .split('/')
            .any(|segment| segment.starts_with("user@") && segment.ends_with(".service"));
    }
    if user_manager {
        Signal::Absent
    } else {
        Signal::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_unit_is_a_login_start() {
        let text = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-com.example.notes@autostart.service\n";
        assert_eq!(signal(text), Signal::Login);
    }

    #[test]
    fn app_scope_of_the_user_manager_is_a_user_start() {
        let text = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-com.example.notes-4242.scope\n";
        assert_eq!(signal(text), Signal::Absent);
    }

    #[test]
    fn session_scope_says_nothing() {
        assert_eq!(
            signal("0::/user.slice/user-1000.slice/session-2.scope\n"),
            Signal::Unavailable
        );
        assert_eq!(signal("0::/\n"), Signal::Unavailable);
        assert_eq!(signal(""), Signal::Unavailable);
    }

    #[test]
    fn cgroup_v1_lines_are_read() {
        let text = "12:pids:/user.slice/user-1000.slice/user@1000.service/app.slice/app-notes@autostart.service\n1:name=systemd:/user.slice/user-1000.slice/user@1000.service/app.slice/app-notes@autostart.service\n";
        assert_eq!(signal(text), Signal::Login);
        let text =
            "1:name=systemd:/user.slice/user-1000.slice/user@1000.service/app.slice/x.scope\n";
        assert_eq!(signal(text), Signal::Absent);
    }

    #[test]
    fn autostart_in_the_middle_of_the_path_does_not_count() {
        let text = "0::/user.slice/user@1000.service/app-x@autostart.service/sub.scope\n";
        assert_eq!(signal(text), Signal::Absent);
    }
}
