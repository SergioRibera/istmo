//! The two monitor names an EDID block carries cheaply: the PNP
//! manufacturer id and the monitor-name descriptor.

const HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];
/// Offsets of the four 18-byte descriptors of the base block.
const DESCRIPTORS: [usize; 4] = [54, 72, 90, 108];
const DESCRIPTOR_LEN: usize = 18;
/// Display descriptor tag: monitor name.
const MONITOR_NAME: u8 = 0xfc;

/// What an EDID names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Edid {
    /// Three-letter PNP id (`DEL`, `SAM`).
    pub(super) make: Option<String>,
    pub(super) model: Option<String>,
}

impl Edid {
    /// `None` for anything that is not an EDID base block.
    pub(super) fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.get(..HEADER.len())? != HEADER {
            return None;
        }
        let id = u16::from_be_bytes([*bytes.get(8)?, *bytes.get(9)?]);
        let letters: Option<String> = [10_u16, 5, 0]
            .into_iter()
            .map(|shift| {
                let code = u8::try_from((id >> shift) & 0x1f).ok()?;
                // 1 = 'A' .. 26 = 'Z'; anything else is not a PNP id.
                (1..=26).contains(&code).then(|| char::from(b'@' + code))
            })
            .collect();
        let model = DESCRIPTORS.into_iter().find_map(|at| {
            let descriptor = bytes.get(at..at + DESCRIPTOR_LEN)?;
            if descriptor.get(..4) != Some(&[0, 0, 0, MONITOR_NAME]) {
                return None;
            }
            let text = descriptor.get(5..)?;
            let end = text.iter().position(|b| *b == b'\n').unwrap_or(text.len());
            let name = String::from_utf8_lossy(text.get(..end)?).trim().to_owned();
            (!name.is_empty()).then_some(name)
        });
        Some(Self {
            make: letters,
            model,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A base block with PNP id `DEL` and monitor name `DELL U2720Q`.
    fn sample() -> Vec<u8> {
        let mut edid = vec![0_u8; 128];
        edid[..8].copy_from_slice(&HEADER);
        // D=4, E=5, L=12: 0b0_00100_00101_01100.
        let id: u16 = (4 << 10) | (5 << 5) | 0b1100;
        edid[8..10].copy_from_slice(&id.to_be_bytes());
        // Descriptor 1: a serial number (0xff), descriptor 2: the name.
        edid[54..59].copy_from_slice(&[0, 0, 0, 0xff, 0]);
        edid[72..77].copy_from_slice(&[0, 0, 0, MONITOR_NAME, 0]);
        let name = b"DELL U2720Q\n ";
        edid[77..77 + name.len()].copy_from_slice(name);
        edid
    }

    #[test]
    fn make_and_model_are_read() {
        assert_eq!(
            Edid::parse(&sample()),
            Some(Edid {
                make: Some("DEL".to_owned()),
                model: Some("DELL U2720Q".to_owned()),
            })
        );
    }

    #[test]
    fn anything_else_is_refused_or_partial() {
        assert_eq!(Edid::parse(&[0; 128]), None, "no header");
        assert_eq!(Edid::parse(&HEADER), None, "truncated");
        let mut nameless = sample();
        nameless[72..90].fill(0x20);
        assert_eq!(
            Edid::parse(&nameless),
            Some(Edid {
                make: Some("DEL".to_owned()),
                model: None,
            })
        );
    }
}
