/// A menu label with its mnemonic markup parsed: `&x` marks `x` as the
/// mnemonic, `&&` is a literal `&`. Only the first marker counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    text: String,
    /// Byte offset of the mnemonic character in `text`.
    mnemonic: Option<usize>,
}

impl Label {
    /// Parses `source` (a [`MenuNode::label`](crate::MenuNode::label)).
    #[must_use]
    pub fn parse(source: &str) -> Self {
        let mut text = String::with_capacity(source.len());
        let mut mnemonic = None;
        let mut chars = source.chars();
        while let Some(c) = chars.next() {
            if c != '&' {
                text.push(c);
                continue;
            }
            match chars.next() {
                // `&&` is a literal `&`; a trailing `&` marks nothing.
                Some('&') | None => text.push('&'),
                Some(next) => {
                    if mnemonic.is_none() {
                        mnemonic = Some(text.len());
                    }
                    text.push(next);
                }
            }
        }
        Self { text, mnemonic }
    }

    /// The text without markup.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The mnemonic character, if one is marked.
    #[must_use]
    pub fn mnemonic(&self) -> Option<char> {
        self.mnemonic.and_then(|at| self.text[at..].chars().next())
    }

    /// The label with the mnemonic marked by `marker` and every literal
    /// `marker` doubled: `'&'` for Win32 menus, `'_'` for dbusmenu.
    #[must_use]
    pub fn with_marker(&self, marker: char) -> String {
        let mut out = String::with_capacity(self.text.len() + 2);
        for (at, c) in self.text.char_indices() {
            if Some(at) == self.mnemonic {
                out.push(marker);
            }
            if c == marker {
                out.push(marker);
            }
            out.push(c);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mnemonic_and_literal_ampersand() {
        let label = Label::parse("Save && &Quit");
        assert_eq!(label.text(), "Save & Quit");
        assert_eq!(label.mnemonic(), Some('Q'));
        assert_eq!(label.with_marker('&'), "Save && &Quit");
        assert_eq!(label.with_marker('_'), "Save & _Quit");
    }

    #[test]
    fn only_the_first_marker_counts() {
        let label = Label::parse("&a&b");
        assert_eq!(label.text(), "ab");
        assert_eq!(label.mnemonic(), Some('a'));
    }

    #[test]
    fn underscores_are_doubled_for_dbusmenu() {
        let label = Label::parse("snake_case &file");
        assert_eq!(label.with_marker('_'), "snake__case _file");
    }

    #[test]
    fn trailing_ampersand_is_text() {
        let label = Label::parse("R&");
        assert_eq!(label.text(), "R&");
        assert_eq!(label.mnemonic(), None);
    }

    #[test]
    fn multibyte_mnemonic() {
        let label = Label::parse("&Ñandú");
        assert_eq!(label.mnemonic(), Some('Ñ'));
        assert_eq!(label.with_marker('_'), "_Ñandú");
    }
}
