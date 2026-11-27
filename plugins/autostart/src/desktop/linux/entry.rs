//! XDG desktop entry files, as far as autostart needs them: writing an
//! entry, reading `Exec`, `Hidden` and `X-GNOME-Autostart-enabled`
//! back, and replacing `Exec` in place.
//!
//! `Exec` follows the Desktop Entry Specification: arguments with
//! reserved characters are double-quoted (`"`, `` ` ``, `$` and `\`
//! backslash-escaped inside), `%` is doubled, and the whole value then
//! gets the `string` escapes (`\\`, `\n`, `\t`, `\r`).

use std::fmt::Write as _;

use crate::desktop::LaunchCommand;

const GROUP: &str = "[Desktop Entry]";

/// The keys of the `[Desktop Entry]` group autostart reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Entry {
    /// `Exec`, split into argv (field codes dropped).
    pub(crate) exec: Option<Vec<String>>,
    /// `Hidden=true`: deleted by the user (how Plasma and others turn an
    /// autostart entry off).
    pub(crate) hidden: bool,
    /// `X-GNOME-Autostart-enabled=false`.
    pub(crate) gnome_disabled: bool,
}

impl Entry {
    /// Read the `[Desktop Entry]` group of `text`.
    pub(crate) fn parse(text: &str) -> Self {
        let mut entry = Self::default();
        let mut in_group = false;
        for line in text.lines() {
            let line = line.trim_start();
            if line.starts_with('[') {
                in_group = line.trim_end() == GROUP;
                continue;
            }
            if !in_group || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim_end() {
                "Exec" => entry.exec = Some(parse_exec(value)),
                "Hidden" => entry.hidden = value == "true",
                "X-GNOME-Autostart-enabled" => entry.gnome_disabled = value == "false",
                _ => {}
            }
        }
        entry
    }

    /// Whether the user turned the entry off.
    pub(crate) const fn disabled_by_user(&self) -> bool {
        self.hidden || self.gnome_disabled
    }
}

/// A complete autostart entry for `command`.
pub(crate) fn render(name: &str, command: &LaunchCommand) -> Option<String> {
    let exec = render_exec(command)?;
    let name = escape_string(name);
    Some(format!(
        "{GROUP}\nType=Application\nName={name}\nExec={exec}\nX-GNOME-Autostart-enabled=true\nX-Istmo-Autostart=true\n"
    ))
}

/// `text` with the `Exec` line of its `[Desktop Entry]` group replaced
/// (or added to the group), every other line kept as it was.
pub(crate) fn replace_exec(text: &str, command: &LaunchCommand) -> Option<String> {
    let exec = render_exec(command)?;
    let mut out = String::with_capacity(text.len() + exec.len());
    let mut in_group = false;
    let mut written = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            if in_group && !written {
                let _ = writeln!(out, "Exec={exec}");
                written = true;
            }
            in_group = trimmed.trim_end() == GROUP;
        } else if in_group
            && trimmed
                .split_once('=')
                .is_some_and(|(key, _)| key.trim_end() == "Exec")
        {
            if !written {
                let _ = writeln!(out, "Exec={exec}");
                written = true;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !written {
        if !in_group {
            out.push_str(GROUP);
            out.push('\n');
        }
        let _ = writeln!(out, "Exec={exec}");
    }
    Some(out)
}

fn render_exec(command: &LaunchCommand) -> Option<String> {
    let mut raw = String::new();
    quote_arg(command.program.to_str()?, &mut raw);
    for arg in &command.args {
        raw.push(' ');
        quote_arg(arg, &mut raw);
    }
    Some(escape_string(&raw))
}

/// Characters the spec reserves outside quotes.
const fn is_reserved(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t'
            | '\n'
            | '"'
            | '\''
            | '\\'
            | '>'
            | '<'
            | '~'
            | '|'
            | '&'
            | ';'
            | '$'
            | '*'
            | '?'
            | '#'
            | '('
            | ')'
            | '`'
    )
}

fn quote_arg(arg: &str, out: &mut String) {
    let quote = arg.is_empty() || arg.chars().any(is_reserved);
    if quote {
        out.push('"');
    }
    for c in arg.chars() {
        match c {
            '%' => out.push_str("%%"),
            '"' | '`' | '$' | '\\' if quote => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    if quote {
        out.push('"');
    }
}

/// The `string` value escapes.
fn escape_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

fn unescape_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Split an `Exec` value into argv. Field codes (`%f`, `%U`, …) are
/// dropped, `%%` is a `%`.
pub(crate) fn parse_exec(value: &str) -> Vec<String> {
    let raw = unescape_string(value);
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut field_code_only = true;
    let mut chars = raw.chars().peekable();
    let mut finish = |current: &mut String, in_token: &mut bool, field_code_only: &mut bool| {
        if *in_token && !(*field_code_only && current.is_empty()) {
            args.push(std::mem::take(current));
        }
        current.clear();
        *in_token = false;
        *field_code_only = true;
    };
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => finish(&mut current, &mut in_token, &mut field_code_only),
            '"' => {
                in_token = true;
                field_code_only = false;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => {
                            if let Some(escaped) = chars.next() {
                                current.push(escaped);
                            }
                        }
                        '%' if chars.peek() == Some(&'%') => {
                            chars.next();
                            current.push('%');
                        }
                        _ => current.push(q),
                    }
                }
            }
            '%' => {
                in_token = true;
                // Anything else after `%` is a field code: it expands to
                // nothing here.
                if chars.next() == Some('%') {
                    field_code_only = false;
                    current.push('%');
                }
            }
            _ => {
                in_token = true;
                field_code_only = false;
                current.push(c);
            }
        }
    }
    finish(&mut current, &mut in_token, &mut field_code_only);
    args
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn command(program: &str, args: &[&str]) -> LaunchCommand {
        LaunchCommand {
            program: PathBuf::from(program),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    #[test]
    fn exec_round_trips_awkward_arguments() {
        let cmd = command(
            "/home/ana/My Apps/notes",
            &[
                "--autostart",
                "50%",
                "a\"b",
                "c\\d",
                "$HOME",
                "",
                "tab\there",
            ],
        );
        let exec = render_exec(&cmd).unwrap();
        let mut expected = vec![cmd.program.to_str().unwrap().to_owned()];
        expected.extend(cmd.args.iter().cloned());
        assert_eq!(parse_exec(&exec), expected);
    }

    #[test]
    fn plain_arguments_stay_unquoted() {
        let exec = render_exec(&command("/usr/bin/notes", &["--autostart"])).unwrap();
        assert_eq!(exec, "/usr/bin/notes --autostart");
        let exec = render_exec(&command("/opt/a b/notes", &["x"])).unwrap();
        assert_eq!(exec, "\"/opt/a b/notes\" x");
        let exec = render_exec(&command("/opt/c\\d", &[])).unwrap();
        // Quoted-arg escape, then string escape.
        assert_eq!(exec, "\"/opt/c\\\\\\\\d\"");
    }

    #[test]
    fn field_codes_are_dropped() {
        assert_eq!(parse_exec("notes %U --x %%f"), ["notes", "--x", "%f"]);
    }

    #[test]
    fn parse_reads_only_the_desktop_entry_group() {
        let text = "# c\n[Desktop Entry]\nExec=notes --a\nHidden=true\n[Desktop Action x]\nExec=other\nX-GNOME-Autostart-enabled=false\n";
        let entry = Entry::parse(text);
        assert_eq!(entry.exec, Some(vec!["notes".to_owned(), "--a".to_owned()]));
        assert!(entry.hidden);
        assert!(!entry.gnome_disabled);
        assert!(entry.disabled_by_user());
        let gnome = Entry::parse("[Desktop Entry]\nX-GNOME-Autostart-enabled=false\n");
        assert!(gnome.disabled_by_user());
    }

    #[test]
    fn replace_exec_keeps_every_other_line() {
        let text = "[Desktop Entry]\nType=Application\nName=Notes\nExec=/old/notes\nX-KDE-Foo=1\n[Desktop Action a]\nExec=/old/notes --a\n";
        let out = replace_exec(text, &command("/new/notes", &["--autostart"])).unwrap();
        assert_eq!(
            out,
            "[Desktop Entry]\nType=Application\nName=Notes\nExec=/new/notes --autostart\nX-KDE-Foo=1\n[Desktop Action a]\nExec=/old/notes --a\n"
        );
        let missing = replace_exec("[Desktop Entry]\nName=N\n", &command("/n", &[])).unwrap();
        assert_eq!(missing, "[Desktop Entry]\nName=N\nExec=/n\n");
    }

    #[test]
    fn render_parses_back() {
        let cmd = command("/usr/bin/notes", &["--autostart"]);
        let text = render("Notes\nInjected=1", &cmd).unwrap();
        assert!(!text.contains("\nInjected=1"));
        let entry = Entry::parse(&text);
        assert_eq!(
            entry.exec,
            Some(vec!["/usr/bin/notes".to_owned(), "--autostart".to_owned()])
        );
        assert!(!entry.disabled_by_user());
    }
}
