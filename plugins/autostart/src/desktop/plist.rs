//! The `LaunchAgent` property list (`~/Library/LaunchAgents/<label>.plist`)
//! used where `SMAppService` is not available.
//!
//! Only the shape this plugin writes is read back: the strings of the
//! `ProgramArguments` array.

use crate::desktop::LaunchCommand;

/// The plist for `label` running `command` at login.
pub(crate) fn render(label: &str, command: &LaunchCommand) -> Option<String> {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>",
    );
    escape(label, &mut out);
    out.push_str("</string>\n\t<key>ProgramArguments</key>\n\t<array>\n");
    for arg in
        std::iter::once(command.program.to_str()?).chain(command.args.iter().map(String::as_str))
    {
        out.push_str("\t\t<string>");
        escape(arg, &mut out);
        out.push_str("</string>\n");
    }
    out.push_str(
        "\t</array>\n\t<key>RunAtLoad</key>\n\t<true/>\n\
         \t<key>ProcessType</key>\n\t<string>Interactive</string>\n</dict>\n</plist>\n",
    );
    Some(out)
}

/// The `ProgramArguments` strings, `None` when the array is missing.
pub(crate) fn program_arguments(text: &str) -> Option<Vec<String>> {
    let after_key = text.split_once("<key>ProgramArguments</key>")?.1;
    let array = after_key.trim_start().strip_prefix("<array>")?;
    let array = array.split_once("</array>")?.0;
    let mut args = Vec::new();
    let mut rest = array;
    while let Some((_, after)) = rest.split_once("<string>") {
        let (value, tail) = after.split_once("</string>")?;
        args.push(unescape(value));
        rest = tail;
    }
    Some(args)
}

fn escape(raw: &str, out: &mut String) {
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

fn unescape(raw: &str) -> String {
    raw.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn arguments_round_trip() {
        let command = LaunchCommand {
            program: PathBuf::from("/Applications/Notes & Co.app/Contents/MacOS/notes"),
            args: vec!["--autostart".to_owned(), "<x>".to_owned()],
        };
        let text = render("com.example.notes.istmo-autostart", &command).unwrap();
        assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(text.contains("<string>Interactive</string>"));
        assert_eq!(
            program_arguments(&text).unwrap(),
            [
                "/Applications/Notes & Co.app/Contents/MacOS/notes",
                "--autostart",
                "<x>"
            ]
        );
    }

    #[test]
    fn missing_array_is_none() {
        assert_eq!(program_arguments("<plist><dict></dict></plist>"), None);
    }
}
