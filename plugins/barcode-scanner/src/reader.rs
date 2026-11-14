//! Frame decoder for apps that draw their own scanner.
//!
//! [`BarcodeScanner::scan`](crate::BarcodeScanner::scan) hands the whole
//! screen to the platform scanner. An app that wants its own preview
//! and overlay opens the camera with `istmo-camera`, draws the frames it
//! receives, and passes the same frames to a [`BarcodeReader`]: each
//! [`DetectedCode`] carries its corners in frame pixels, ready to draw.
//! Decoding is pure Rust (`rxing`), so the same code runs on every
//! platform the camera does.
//!
//! No symbology is compiled in by default: enable the `reader-*`
//! features the app reads (`reader-qr`, `reader-1d`,
//! `reader-datamatrix`, `reader-pdf417`, `reader-aztec`), so the binary
//! only carries those decoders. [`BarcodeReader::supports`] tells which
//! ones are in.
//!
//! ```rust,ignore
//! let reader = BarcodeReader::default().with_formats([BarcodeFormat::Qr]);
//! while let Ok(frame) = frames.recv() {
//!     draw_preview(&frame);
//!     for code in reader.read(&frame)? {
//!         draw_outline(&code.corners, frame.rotation_degrees);
//!     }
//! }
//! ```
//!
//! [`BarcodeReader::read`] blocks for one decode pass (milliseconds at
//! preview resolutions, more with
//! [`with_try_harder`](BarcodeReader::with_try_harder)). Run it off the UI
//! thread and skip frames while a pass is still running.

use std::collections::HashSet;
use std::fmt;

use istmo_camera::{CameraFrame, FramePlane, PixelFormat};
use rxing::{BarcodeFormat as RxFormat, DecodeHints, Exceptions};

use crate::{BarcodeFormat, ScannedCode};

#[cfg(not(any(
    feature = "reader-qr",
    feature = "reader-1d",
    feature = "reader-datamatrix",
    feature = "reader-pdf417",
    feature = "reader-aztec",
)))]
compile_error!(
    "istmo-barcode-scanner: `reader` decodes no symbology on its own; enable the ones the app \
     reads: `reader-qr`, `reader-1d`, `reader-datamatrix`, `reader-pdf417`, `reader-aztec`"
);

/// A point in frame pixels: `x` to the right, `y` down, in the frame's
/// buffer orientation (rotate it by
/// [`CameraFrame::rotation_degrees`] exactly like the preview).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FramePoint {
    /// Column, in pixels.
    pub x: f32,
    /// Row, in pixels.
    pub y: f32,
}

/// A code found in a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectedCode {
    /// What the code holds.
    pub code: ScannedCode,
    /// Points the decoder located the code by, in frame pixels. Four
    /// corners for most 2D codes (QR codes report their three finder
    /// patterns and, when found, an alignment pattern); the two ends of
    /// the scan line for 1D codes.
    pub corners: Vec<FramePoint>,
}

/// Why a frame could not be decoded. Finding no code is not an error:
/// [`BarcodeReader::read`] returns an empty list.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadError {
    /// The frame is encoded ([`PixelFormat::Jpeg`]); stream raw frames.
    UnsupportedPixelFormat(PixelFormat),
    /// The frame's planes do not fit its bytes, or it has no plane.
    MalformedFrame,
    /// [`BarcodeReader::with_formats`] asked for a symbology whose
    /// `reader-*` feature is off.
    FormatNotCompiled(BarcodeFormat),
    /// The decoder failed for another reason.
    Decoder(String),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPixelFormat(format) => {
                write!(f, "cannot decode barcodes in {format:?} frames")
            }
            Self::MalformedFrame => f.write_str("frame planes do not match its bytes"),
            Self::FormatNotCompiled(format) => write!(
                f,
                "{format:?} is not compiled in; enable its `reader-*` feature"
            ),
            Self::Decoder(msg) => write!(f, "barcode decoder: {msg}"),
        }
    }
}

impl std::error::Error for ReadError {}

/// Decodes barcodes in camera frames.
#[derive(Debug, Clone, Default)]
pub struct BarcodeReader {
    /// Empty reads every compiled symbology.
    formats: Vec<BarcodeFormat>,
    try_harder: bool,
}

impl BarcodeReader {
    /// Only look for these symbologies; fewer formats decode faster.
    /// [`BarcodeFormat::Other`] is ignored, and an empty list reads every
    /// compiled one. Each must be compiled in (see [`Self::supports`]),
    /// or [`read`](Self::read) fails.
    #[must_use]
    pub fn with_formats(mut self, formats: impl IntoIterator<Item = BarcodeFormat>) -> Self {
        self.formats = formats.into_iter().collect();
        self
    }

    /// Spend more time per frame to find small, blurry or skewed
    /// codes. Off by default: live preview frames come again soon.
    #[must_use]
    pub const fn with_try_harder(mut self, try_harder: bool) -> Self {
        self.try_harder = try_harder;
        self
    }

    /// Whether `format` is compiled in (its `reader-*` feature is on).
    #[must_use]
    pub const fn supports(format: BarcodeFormat) -> bool {
        match format {
            BarcodeFormat::Qr => cfg!(feature = "reader-qr"),
            BarcodeFormat::Ean13
            | BarcodeFormat::Ean8
            | BarcodeFormat::UpcA
            | BarcodeFormat::UpcE
            | BarcodeFormat::Code128
            | BarcodeFormat::Code39
            | BarcodeFormat::Code93
            | BarcodeFormat::Itf
            | BarcodeFormat::Codabar => cfg!(feature = "reader-1d"),
            BarcodeFormat::DataMatrix => cfg!(feature = "reader-datamatrix"),
            BarcodeFormat::Pdf417 => cfg!(feature = "reader-pdf417"),
            BarcodeFormat::Aztec => cfg!(feature = "reader-aztec"),
            BarcodeFormat::Other => false,
        }
    }

    /// Every code found in `frame`, possibly none.
    ///
    /// # Errors
    ///
    /// [`ReadError::FormatNotCompiled`] when a requested symbology's
    /// feature is off, [`ReadError::UnsupportedPixelFormat`] for JPEG
    /// frames, [`ReadError::MalformedFrame`] when the planes do not fit
    /// the bytes, [`ReadError::Decoder`] for other decoder failures.
    pub fn read(&self, frame: &CameraFrame) -> Result<Vec<DetectedCode>, ReadError> {
        if let Some(missing) = self
            .formats
            .iter()
            .find(|format| **format != BarcodeFormat::Other && !Self::supports(**format))
        {
            return Err(ReadError::FormatNotCompiled(*missing));
        }
        let luma = Luma::from_frame(frame)?;
        let mut hints = DecodeHints {
            TryHarder: Some(self.try_harder),
            ..DecodeHints::default()
        };
        let wanted: HashSet<RxFormat> = self.formats.iter().filter_map(|f| to_rx(*f)).collect();
        if !wanted.is_empty() {
            hints.PossibleFormats = Some(wanted);
        } else if !self.formats.is_empty() {
            // Only `Other` was asked for: it never matches.
            return Ok(Vec::new());
        }
        match rxing::helpers::detect_multiple_in_luma_with_hints(
            luma.bytes,
            luma.width,
            luma.height,
            &mut hints,
        ) {
            Ok(results) => Ok(results
                .iter()
                .map(|result| DetectedCode {
                    code: ScannedCode {
                        text: result.getText().to_owned(),
                        format: from_rx(*result.getBarcodeFormat()),
                        raw: Some(result.getRawBytes())
                            .filter(|raw| !raw.is_empty())
                            .map(<[u8]>::to_vec),
                    },
                    corners: result
                        .getPoints()
                        .iter()
                        .map(|point| FramePoint {
                            x: point.x,
                            y: point.y,
                        })
                        .collect(),
                })
                .collect()),
            Err(Exceptions::NotFoundException(_)) => Ok(Vec::new()),
            Err(err) => Err(ReadError::Decoder(err.to_string())),
        }
    }
}

/// A tightly packed 8-bit luminance image.
struct Luma {
    bytes: Vec<u8>,
    width: u32,
    height: u32,
}

impl Luma {
    fn from_frame(frame: &CameraFrame) -> Result<Self, ReadError> {
        let plane = frame.planes.first().ok_or(ReadError::MalformedFrame)?;
        match frame.pixel_format {
            // The first plane of both YUV layouts is luminance.
            PixelFormat::Nv21 | PixelFormat::Yuv420 => {
                Self::from_plane(&frame.bytes, plane, 1, |px| px[0])
            }
            PixelFormat::Rgba8 => {
                Self::from_plane(&frame.bytes, plane, 4, |px| luma(px[0], px[1], px[2]))
            }
            PixelFormat::Bgra8 => {
                Self::from_plane(&frame.bytes, plane, 4, |px| luma(px[2], px[1], px[0]))
            }
            format @ PixelFormat::Jpeg(_) => Err(ReadError::UnsupportedPixelFormat(format)),
        }
    }

    /// Copy `plane` into a packed buffer, turning each `sample_len`
    /// byte sample into one luminance byte.
    fn from_plane(
        bytes: &[u8],
        plane: &FramePlane,
        sample_len: usize,
        to_luma: impl Fn(&[u8]) -> u8,
    ) -> Result<Self, ReadError> {
        let (width, height) = (plane.width as usize, plane.height as usize);
        let (offset, row_stride, pixel_stride) = (
            plane.offset as usize,
            plane.row_stride as usize,
            plane.pixel_stride as usize,
        );
        if width == 0 || height == 0 || pixel_stride < sample_len {
            return Err(ReadError::MalformedFrame);
        }
        let mut out = Vec::with_capacity(width * height);
        for row in 0..height {
            let start = row
                .checked_mul(row_stride)
                .and_then(|start| start.checked_add(offset))
                .ok_or(ReadError::MalformedFrame)?;
            let len = (width - 1) * pixel_stride + sample_len;
            let line = start
                .checked_add(len)
                .and_then(|end| bytes.get(start..end))
                .ok_or(ReadError::MalformedFrame)?;
            out.extend(line.chunks(pixel_stride).map(&to_luma));
        }
        Ok(Self {
            bytes: out,
            width: plane.width,
            height: plane.height,
        })
    }
}

/// BT.601 luma in 8.8 fixed point.
fn luma(r: u8, g: u8, b: u8) -> u8 {
    let weighted = 77 * u32::from(r) + 150 * u32::from(g) + 29 * u32::from(b);
    u8::try_from(weighted >> 8).unwrap_or(u8::MAX)
}

const fn to_rx(format: BarcodeFormat) -> Option<RxFormat> {
    Some(match format {
        BarcodeFormat::Qr => RxFormat::QR_CODE,
        BarcodeFormat::Aztec => RxFormat::AZTEC,
        BarcodeFormat::DataMatrix => RxFormat::DATA_MATRIX,
        BarcodeFormat::Pdf417 => RxFormat::PDF_417,
        BarcodeFormat::Code128 => RxFormat::CODE_128,
        BarcodeFormat::Ean13 => RxFormat::EAN_13,
        BarcodeFormat::Ean8 => RxFormat::EAN_8,
        BarcodeFormat::UpcA => RxFormat::UPC_A,
        BarcodeFormat::UpcE => RxFormat::UPC_E,
        BarcodeFormat::Code39 => RxFormat::CODE_39,
        BarcodeFormat::Code93 => RxFormat::CODE_93,
        BarcodeFormat::Itf => RxFormat::ITF,
        BarcodeFormat::Codabar => RxFormat::CODABAR,
        BarcodeFormat::Other => return None,
    })
}

const fn from_rx(format: RxFormat) -> BarcodeFormat {
    match format {
        RxFormat::QR_CODE => BarcodeFormat::Qr,
        RxFormat::AZTEC => BarcodeFormat::Aztec,
        RxFormat::DATA_MATRIX => BarcodeFormat::DataMatrix,
        RxFormat::PDF_417 => BarcodeFormat::Pdf417,
        RxFormat::CODE_128 => BarcodeFormat::Code128,
        RxFormat::EAN_13 => BarcodeFormat::Ean13,
        RxFormat::EAN_8 => BarcodeFormat::Ean8,
        RxFormat::UPC_A => BarcodeFormat::UpcA,
        RxFormat::UPC_E => BarcodeFormat::UpcE,
        RxFormat::CODE_39 => BarcodeFormat::Code39,
        RxFormat::CODE_93 => BarcodeFormat::Code93,
        RxFormat::ITF => BarcodeFormat::Itf,
        RxFormat::CODABAR => BarcodeFormat::Codabar,
        _ => BarcodeFormat::Other,
    }
}

#[cfg(test)]
mod tests {
    use istmo_camera::Resolution;
    use rxing::Writer;

    use super::*;

    const SCALE: u32 = 4;
    const QUIET: u32 = 4;

    /// A QR code holding `text`, as a white-on-black-free luma image.
    fn qr_luma(text: &str) -> (Vec<u8>, u32) {
        let matrix = rxing::MultiFormatWriter
            .encode(text, &RxFormat::QR_CODE, 0, 0)
            .unwrap();
        let side = (matrix.width() + 2 * QUIET) * SCALE;
        let mut luma = vec![255_u8; (side * side) as usize];
        for y in 0..matrix.height() {
            for x in 0..matrix.width() {
                if matrix.get(x, y) {
                    for dy in 0..SCALE {
                        for dx in 0..SCALE {
                            let px = (x + QUIET) * SCALE + dx;
                            let py = (y + QUIET) * SCALE + dy;
                            luma[(py * side + px) as usize] = 0;
                        }
                    }
                }
            }
        }
        (luma, side)
    }

    fn frame(
        pixel_format: PixelFormat,
        side: u32,
        pixel_stride: u32,
        row_pad: u32,
        bytes: Vec<u8>,
    ) -> CameraFrame {
        CameraFrame {
            seq: 0,
            timestamp_ns: 0,
            resolution: Resolution {
                width: side,
                height: side,
            },
            rotation_degrees: 0,
            pixel_format,
            planes: vec![FramePlane {
                offset: 0,
                row_stride: side * pixel_stride + row_pad,
                pixel_stride,
                width: side,
                height: side,
            }],
            bytes,
            dropped_before: 0,
        }
    }

    #[test]
    fn reads_a_qr_code_from_a_padded_luma_plane() {
        let (luma, side) = qr_luma("istmo");
        let pad: u32 = 16;
        let mut bytes = Vec::new();
        for row in luma.chunks(side as usize) {
            bytes.extend_from_slice(row);
            bytes.extend(std::iter::repeat_n(0xAA, usize::try_from(pad).unwrap()));
        }
        // Chroma planes are never read.
        let codes = BarcodeReader::default()
            .read(&frame(PixelFormat::Nv21, side, 1, pad, bytes))
            .unwrap();
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].code.text, "istmo");
        assert_eq!(codes[0].code.format, BarcodeFormat::Qr);
        assert!(codes[0].corners.len() >= 3);
        let max = f32::from(u16::try_from(side).unwrap());
        assert!(
            codes[0]
                .corners
                .iter()
                .all(|p| (0.0..max).contains(&p.x) && (0.0..max).contains(&p.y))
        );
    }

    #[test]
    fn reads_rgba_and_bgra_frames() {
        let (luma, side) = qr_luma("https://istmo.dev");
        let packed: Vec<u8> = luma.iter().flat_map(|&l| [l, l, l, 255]).collect();
        for format in [PixelFormat::Rgba8, PixelFormat::Bgra8] {
            let codes = BarcodeReader::default()
                .with_formats([BarcodeFormat::Qr])
                .read(&frame(format, side, 4, 0, packed.clone()))
                .unwrap();
            assert_eq!(codes[0].code.text, "https://istmo.dev");
        }
    }

    #[test]
    fn empty_frames_and_other_formats_find_nothing() {
        let side = 64;
        let blank = frame(
            PixelFormat::Nv21,
            side,
            1,
            0,
            vec![255; (side * side) as usize],
        );
        assert_eq!(BarcodeReader::default().read(&blank).unwrap(), []);
        let (luma, side) = qr_luma("x");
        let qr = frame(PixelFormat::Nv21, side, 1, 0, luma);
        assert_eq!(
            BarcodeReader::default()
                .with_formats([BarcodeFormat::Ean13])
                .read(&qr)
                .unwrap(),
            [],
            "QR codes are skipped when only EAN-13 is wanted"
        );
        assert_eq!(
            BarcodeReader::default()
                .with_formats([BarcodeFormat::Other])
                .read(&qr)
                .unwrap(),
            []
        );
    }

    #[test]
    fn malformed_and_encoded_frames_are_refused() {
        let short = frame(PixelFormat::Nv21, 32, 1, 0, vec![0; 100]);
        assert_eq!(
            BarcodeReader::default().read(&short),
            Err(ReadError::MalformedFrame)
        );
        let mut no_planes = short;
        no_planes.planes.clear();
        assert_eq!(
            BarcodeReader::default().read(&no_planes),
            Err(ReadError::MalformedFrame)
        );
        let jpeg = frame(PixelFormat::Jpeg(70), 32, 1, 0, vec![0; 32 * 32]);
        assert_eq!(
            BarcodeReader::default().read(&jpeg),
            Err(ReadError::UnsupportedPixelFormat(PixelFormat::Jpeg(70)))
        );
    }

    #[test]
    fn symbology_names_round_trip() {
        for format in [
            BarcodeFormat::Qr,
            BarcodeFormat::Aztec,
            BarcodeFormat::DataMatrix,
            BarcodeFormat::Pdf417,
            BarcodeFormat::Code128,
            BarcodeFormat::Ean13,
            BarcodeFormat::Ean8,
            BarcodeFormat::UpcA,
            BarcodeFormat::UpcE,
            BarcodeFormat::Code39,
            BarcodeFormat::Code93,
            BarcodeFormat::Itf,
            BarcodeFormat::Codabar,
        ] {
            assert_eq!(from_rx(to_rx(format).unwrap()), format);
        }
        assert_eq!(to_rx(BarcodeFormat::Other), None);
        assert_eq!(from_rx(RxFormat::MAXICODE), BarcodeFormat::Other);
    }

    #[test]
    fn compiled_symbologies_are_reported() {
        // The tests compile every `reader-*` feature in.
        assert!(BarcodeReader::supports(BarcodeFormat::Qr));
        assert!(BarcodeReader::supports(BarcodeFormat::Ean13));
        assert!(BarcodeReader::supports(BarcodeFormat::Aztec));
        assert!(!BarcodeReader::supports(BarcodeFormat::Other));
        assert_eq!(
            ReadError::FormatNotCompiled(BarcodeFormat::Qr).to_string(),
            "Qr is not compiled in; enable its `reader-*` feature"
        );
    }
}
