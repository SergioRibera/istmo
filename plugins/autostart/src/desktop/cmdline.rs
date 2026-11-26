//! Windows command lines as the `Run` key stores them, quoted the way
//! `CommandLineToArgvW` and the MSVC runtime split them.

use crate::desktop::LaunchCommand;

/// `"program" args…`. The program is always quoted (a path cannot
/// contain `"`); arguments only when they need it.
pub(crate) fn render(command: &LaunchCommand) -> Option<String> {
    let program = command.program.to_str()?;
    let mut line = String::with_capacity(program.len() + 2);
    line.push('"');
    line.push_str(program);
    line.push('"');
    for arg in &command.args {
        line.push(' ');
        quote(arg, &mut line);
    }
    Some(line)
}

/// One argument, quoted so the MSVC runtime reads it back unchanged.
fn quote(arg: &str, out: &mut String) {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Every backslash before a quote is doubled, then the
                // quote itself is escaped.
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    // Trailing backslashes would escape the closing quote.
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
}

/// Split a stored command line into the program and the rest of the
/// line. The program follows `CommandLineToArgvW`'s rule for the first
/// argument: up to the closing quote, or up to the first space or tab.
pub(crate) fn split_program(line: &str) -> (&str, &str) {
    let line = line.trim_start();
    if let Some(quoted) = line.strip_prefix('"') {
        return quoted
            .split_once('"')
            .map_or((quoted, ""), |(program, rest)| (program, rest));
    }
    line.split_once([' ', '\t']).unwrap_or((line, ""))
}

/// Split the arguments after the program, MSVC style.
pub(crate) fn parse_args(rest: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_arg = false;
    let mut in_quotes = false;
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' if !in_quotes => {
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
            }
            '\\' => {
                in_arg = true;
                let mut backslashes = 1usize;
                while chars.peek() == Some(&'\\') {
                    chars.next();
                    backslashes += 1;
                }
                if chars.peek() == Some(&'"') {
                    current.extend(std::iter::repeat_n('\\', backslashes / 2));
                    if backslashes % 2 == 1 {
                        chars.next();
                        current.push('"');
                    }
                } else {
                    current.extend(std::iter::repeat_n('\\', backslashes));
                }
            }
            '"' => {
                in_arg = true;
                if in_quotes && chars.peek() == Some(&'"') {
                    // `""` inside quotes is a literal quote.
                    chars.next();
                    current.push('"');
                } else {
                    in_quotes = !in_quotes;
                }
            }
            _ => {
                in_arg = true;
                current.push(c);
            }
        }
    }
    if in_arg {
        args.push(current);
    }
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
    fn simple_line() {
        let line = render(&command(
            r"C:\Program Files\Notes\notes.exe",
            &["--autostart"],
        ))
        .unwrap();
        assert_eq!(line, r#""C:\Program Files\Notes\notes.exe" --autostart"#);
        assert_eq!(
            split_program(&line),
            (r"C:\Program Files\Notes\notes.exe", " --autostart")
        );
    }

    #[test]
    fn awkward_arguments_round_trip() {
        let args = [
            "",
            "a b",
            r#"say "hi""#,
            r"C:\dir\",
            r#"back\"slash"#,
            r"\\server\share",
            "plain",
        ];
        let line = render(&command(r"C:\n.exe", &args)).unwrap();
        let (program, rest) = split_program(&line);
        assert_eq!(program, r"C:\n.exe");
        assert_eq!(parse_args(rest), args);
    }

    #[test]
    fn unquoted_program_ends_at_whitespace() {
        assert_eq!(split_program(r"C:\n.exe --x"), (r"C:\n.exe", "--x"));
        assert_eq!(split_program(r"C:\n.exe"), (r"C:\n.exe", ""));
        assert_eq!(split_program(r#""C:\n.exe"#), (r"C:\n.exe", ""));
    }
}
