//! `.ico` files for Jump List task icons.
//!
//! A shell link names its icon by file path, so a [`crate::ActionIcon::Png`]
//! becomes a one-image `.ico` that embeds the PNG as is (the format Windows
//! Vista and later read for every size), named by a hash of the bytes so
//! an unchanged icon is written once.

use std::path::{Path, PathBuf};

/// Size of the ICONDIR header plus one ICONDIRENTRY.
const HEADER_LEN: usize = 6 + 16;

/// A `.ico` holding `png`, or `None` when `png` has no readable IHDR.
pub(crate) fn from_png(png: &[u8]) -> Option<Vec<u8>> {
    let width = u32::from_be_bytes(png.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(png.get(20..24)?.try_into().ok()?);
    // 256 and larger are written as 0; Windows reads the PNG's own size.
    let edge = |px: u32| u8::try_from(px).ok().filter(|&b| b != 0).unwrap_or(0);
    let len = u32::try_from(png.len()).ok()?;
    let mut ico = Vec::with_capacity(HEADER_LEN + png.len());
    ico.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    ico.extend_from_slice(&1u16.to_le_bytes()); // one image
    ico.push(edge(width));
    ico.push(edge(height));
    ico.push(0); // no palette
    ico.push(0); // reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // planes
    ico.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    ico.extend_from_slice(&len.to_le_bytes());
    ico.extend_from_slice(&u32::try_from(HEADER_LEN).ok()?.to_le_bytes());
    ico.extend_from_slice(png);
    Some(ico)
}

/// FNV-1a 64 of `bytes`: stable across runs, which `DefaultHasher` is not.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Path of the `.ico` for `png` under `dir`, written when missing.
pub(crate) fn materialise(dir: &Path, png: &[u8]) -> std::io::Result<PathBuf> {
    let path = dir.join(format!("{:016x}.ico", fnv1a(png)));
    if !path.is_file() {
        let ico = from_png(png).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "icon is not a PNG")
        })?;
        std::fs::create_dir_all(dir)?;
        // Write then rename, so a concurrent reader never sees half a file.
        let partial = path.with_extension("ico.partial");
        std::fs::write(&partial, ico)?;
        std::fs::rename(&partial, &path)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        png
    }

    #[test]
    fn embeds_the_png_after_one_entry() {
        let source = png(32, 48);
        let ico = from_png(&source).expect("ico");
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 1, 0]);
        assert_eq!((ico[6], ico[7]), (32, 48));
        assert_eq!(
            &ico[14..18],
            &u32::try_from(source.len()).unwrap().to_le_bytes()
        );
        assert_eq!(&ico[18..22], &22u32.to_le_bytes());
        assert_eq!(&ico[22..], source.as_slice());
    }

    #[test]
    fn large_edges_are_written_as_zero() {
        let ico = from_png(&png(256, 512)).expect("ico");
        assert_eq!((ico[6], ico[7]), (0, 0));
    }

    #[test]
    fn short_input_is_refused() {
        assert_eq!(from_png(b"\x89PNG"), None);
    }

    #[test]
    fn materialise_is_stable_by_content() {
        let dir = std::env::temp_dir().join(format!("istmo-la-ico-{}", std::process::id()));
        let a = materialise(&dir, &png(16, 16)).expect("write");
        let b = materialise(&dir, &png(16, 16)).expect("reuse");
        assert_eq!(a, b);
        assert_ne!(a, materialise(&dir, &png(24, 24)).expect("other"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
