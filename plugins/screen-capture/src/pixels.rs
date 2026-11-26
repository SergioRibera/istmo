//! CPU pixel helper: crop, orientation and byte-order conversion for
//! screenshots read as raw pixels. Never encodes, never tone-maps.

use std::fmt;

use crate::types::{CpuPixelFormat, Orientation, PixelFormat, PixelLayout, Rect, Rotation, Size};

/// Why pixels could not be converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PixelError {
    /// The source format is not a packed RGB format this helper reads.
    UnsupportedFormat(PixelFormat),
    /// Converting would change the dynamic range (HDR ↔ SDR).
    ToneMapping {
        from: PixelFormat,
        to: CpuPixelFormat,
    },
    /// The buffer is shorter than `stride × height`.
    ShortBuffer { needed: usize, len: usize },
    /// The stride cannot hold a row.
    BadStride { stride: u32, row: u64 },
    /// The crop lies outside the buffer.
    CropOutside { crop: Rect, size: Size },
    /// The image would not fit in memory.
    TooLarge,
}

impl fmt::Display for PixelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFormat(format) => write!(f, "{format:?} cannot be read on the CPU"),
            Self::ToneMapping { from, to } => {
                write!(
                    f,
                    "{from:?} to {to:?} would tone-map, which the plugin never does"
                )
            }
            Self::ShortBuffer { needed, len } => {
                write!(f, "pixel buffer holds {len} bytes, {needed} needed")
            }
            Self::BadStride { stride, row } => {
                write!(f, "stride {stride} is shorter than a {row}-byte row")
            }
            Self::CropOutside { crop, size } => {
                write!(
                    f,
                    "crop {crop:?} lies outside a {}x{} buffer",
                    size.width, size.height
                )
            }
            Self::TooLarge => f.write_str("image too large"),
        }
    }
}

impl std::error::Error for PixelError {}

/// Maps a pixel of an oriented image back to the buffer it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrientationMap {
    /// Buffer (pre-orientation) size.
    pub size: Size,
    pub orientation: Orientation,
}

impl OrientationMap {
    /// Size once oriented.
    #[must_use]
    pub const fn output_size(&self) -> Size {
        self.orientation.applied_size(self.size)
    }

    /// The buffer rectangle shown by `upright` (a rectangle of the
    /// oriented image), clamped to the buffer.
    #[must_use]
    pub fn buffer_rect(&self, upright: Rect) -> Rect {
        let out = self.output_size();
        let Some(upright) = upright.intersect(Rect::from_size(out)) else {
            return Rect::default();
        };
        let x1 = upright.x.unsigned_abs() + upright.width - 1;
        let y1 = upright.y.unsigned_abs() + upright.height - 1;
        let (ax, ay) = self.source(upright.x.unsigned_abs(), upright.y.unsigned_abs());
        let (bx, by) = self.source(x1, y1);
        let (x0, x1) = (ax.min(bx), ax.max(bx));
        let (y0, y1) = (ay.min(by), ay.max(by));
        Rect::new(
            i32::try_from(x0).unwrap_or(i32::MAX),
            i32::try_from(y0).unwrap_or(i32::MAX),
            x1 - x0 + 1,
            y1 - y0 + 1,
        )
    }
    /// The buffer coordinate shown at `(u, v)` of the oriented image.
    /// `(u, v)` must lie inside [`output_size`](Self::output_size).
    #[must_use]
    #[expect(
        clippy::many_single_char_names,
        reason = "u, v, w, h, x, y are the usual coordinate names"
    )]
    pub const fn source(&self, u: u32, v: u32) -> (u32, u32) {
        let (w, h) = (self.size.width, self.size.height);
        // Undo the clockwise rotation first (it was applied last).
        let (x, y) = match self.orientation.rotation {
            Rotation::R0 => (u, v),
            Rotation::R90 => (v, h - 1 - u),
            Rotation::R180 => (w - 1 - u, h - 1 - v),
            Rotation::R270 => (w - 1 - v, u),
        };
        if self.orientation.mirrored {
            (w - 1 - x, y)
        } else {
            (x, y)
        }
    }
}

/// A read-only view of CPU pixels.
#[derive(Clone, Copy)]
pub struct CpuView<'a> {
    bytes: &'a [u8],
    size: Size,
    stride: u32,
    format: PixelFormat,
}

impl fmt::Debug for CpuView<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CpuView")
            .field("size", &self.size)
            .field("stride", &self.stride)
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

/// Converted pixels: `stride` bytes per row.
#[derive(Clone, PartialEq, Eq)]
pub struct Pixels {
    pub size: Size,
    pub stride: u32,
    pub format: CpuPixelFormat,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for Pixels {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pixels")
            .field("size", &self.size)
            .field("stride", &self.stride)
            .field("format", &self.format)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "v is at most 1023, so the result is at most 255"
)]
const fn ten_to_eight(v: u32) -> u8 {
    // 10-bit to 8-bit, rounding; v ≤ 1023 so the result fits.
    ((v * 255 + 511) / 1023) as u8
}

impl<'a> CpuView<'a> {
    /// A view of `bytes`, `stride` bytes per row.
    ///
    /// # Errors
    /// Unsupported formats, a stride shorter than a row, a short buffer.
    pub fn new(
        bytes: &'a [u8],
        size: Size,
        stride: u32,
        format: PixelFormat,
    ) -> Result<Self, PixelError> {
        if matches!(format, PixelFormat::Nv12 | PixelFormat::P010) {
            return Err(PixelError::UnsupportedFormat(format));
        }
        let row = u64::from(size.width) * u64::from(format.bytes_per_pixel());
        if u64::from(stride) < row {
            return Err(PixelError::BadStride { stride, row });
        }
        if size.height > 0 {
            let needed = u64::from(stride) * u64::from(size.height - 1) + row;
            let needed = usize::try_from(needed).map_err(|_| PixelError::TooLarge)?;
            if bytes.len() < needed {
                return Err(PixelError::ShortBuffer {
                    needed,
                    len: bytes.len(),
                });
            }
        }
        Ok(Self {
            bytes,
            size,
            stride,
            format,
        })
    }

    #[must_use]
    pub const fn size(&self) -> Size {
        self.size
    }

    #[must_use]
    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    fn offset(&self, x: u32, y: u32) -> usize {
        // In bounds by construction (`new` checked the length, callers
        // stay inside `size`); widened before multiplying.
        let offset = u64::from(y) * u64::from(self.stride)
            + u64::from(x) * u64::from(self.format.bytes_per_pixel());
        usize::try_from(offset).unwrap_or(usize::MAX)
    }

    fn word(&self, x: u32, y: u32) -> u32 {
        let at = self.offset(x, y);
        self.bytes
            .get(at..at + 4)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map_or(0, u32::from_le_bytes)
    }

    /// The pixel at `(x, y)` as straight `R, G, B, A` bytes. Opaque
    /// formats read with alpha 255.
    #[must_use]
    pub fn rgba8(&self, x: u32, y: u32) -> [u8; 4] {
        let at = self.offset(x, y);
        let px = |i: usize| self.bytes.get(at + i).copied().unwrap_or(0);
        match self.format {
            PixelFormat::Bgra8 => [px(2), px(1), px(0), px(3)],
            PixelFormat::Bgrx8 => [px(2), px(1), px(0), 255],
            PixelFormat::Rgba8 => [px(0), px(1), px(2), px(3)],
            PixelFormat::Rgbx8 => [px(0), px(1), px(2), 255],
            PixelFormat::Xr30 | PixelFormat::Ar30 => {
                let w = self.word(x, y);
                let alpha = if self.format == PixelFormat::Ar30 {
                    // 2-bit alpha to 8 bits.
                    u8::try_from((w >> 30) * 85).unwrap_or(u8::MAX)
                } else {
                    255
                };
                [
                    ten_to_eight((w >> 20) & 0x3ff),
                    ten_to_eight((w >> 10) & 0x3ff),
                    ten_to_eight(w & 0x3ff),
                    alpha,
                ]
            }
            PixelFormat::Xb30 => {
                let w = self.word(x, y);
                [
                    ten_to_eight(w & 0x3ff),
                    ten_to_eight((w >> 10) & 0x3ff),
                    ten_to_eight((w >> 20) & 0x3ff),
                    255,
                ]
            }
            PixelFormat::Rgba16f | PixelFormat::Nv12 | PixelFormat::P010 => [0, 0, 0, 255],
        }
    }

    /// The raw 8 bytes of a half-float pixel.
    fn rgba16f(&self, x: u32, y: u32) -> [u8; 8] {
        let at = self.offset(x, y);
        self.bytes
            .get(at..at + 8)
            .and_then(|b| <[u8; 8]>::try_from(b).ok())
            .unwrap_or([0; 8])
    }

    /// The whole buffer turned upright, same format, tight rows:
    /// `(size, stride, bytes)`.
    ///
    /// # Errors
    /// A size that does not fit in memory.
    pub fn orient(&self, orientation: Orientation) -> Result<(Size, u32, Vec<u8>), PixelError> {
        let map = OrientationMap {
            size: self.size,
            orientation,
        };
        let out = map.output_size();
        let bpp = self.format.bytes_per_pixel() as usize;
        let stride = out.width as usize * bpp;
        let len = stride
            .checked_mul(out.height as usize)
            .ok_or(PixelError::TooLarge)?;
        let mut bytes = vec![0u8; len];
        for (v, line) in (0..out.height).zip(bytes.chunks_exact_mut(stride.max(1))) {
            for (u, px) in (0..out.width).zip(line.chunks_exact_mut(bpp)) {
                let (x, y) = map.source(u, v);
                let at = self.offset(x, y);
                if let Some(src) = self.bytes.get(at..at + bpp) {
                    px.copy_from_slice(src);
                }
            }
        }
        Ok((
            out,
            u32::try_from(stride).map_err(|_| PixelError::TooLarge)?,
            bytes,
        ))
    }

    /// Crop to `crop` (buffer pixels), apply `orientation`, and convert
    /// to `layout.format`. Orientation is applied only when
    /// `layout.upright`.
    ///
    /// # Errors
    /// A crop outside the buffer, an HDR ↔ SDR conversion, or a size
    /// that does not fit in memory.
    pub fn convert(
        &self,
        crop: Rect,
        orientation: Orientation,
        layout: PixelLayout,
    ) -> Result<Pixels, PixelError> {
        // An empty crop has no rows to cut (and a zero stride to walk).
        if crop.is_empty() || !Rect::from_size(self.size).contains(crop) || crop.x < 0 || crop.y < 0
        {
            return Err(PixelError::CropOutside {
                crop,
                size: self.size,
            });
        }
        let half_float_in = self.format == PixelFormat::Rgba16f;
        let half_float_out = layout.format == CpuPixelFormat::Rgba16f;
        if half_float_in != half_float_out {
            return Err(PixelError::ToneMapping {
                from: self.format,
                to: layout.format,
            });
        }
        let map = OrientationMap {
            size: crop.size(),
            orientation: if layout.upright {
                orientation
            } else {
                Orientation::UPRIGHT
            },
        };
        let out = map.output_size();
        let bpp = layout.format.bytes_per_pixel();
        let row = out.width.checked_mul(bpp).ok_or(PixelError::TooLarge)?;
        // Untight rows are padded to 64 bytes, a common GPU upload pitch.
        let stride = if layout.tight {
            row
        } else {
            row.div_ceil(64) * 64
        };
        let len = usize::try_from(u64::from(stride) * u64::from(out.height))
            .map_err(|_| PixelError::TooLarge)?;
        let mut bytes = vec![0u8; len];
        let (cx, cy) = (crop.x.unsigned_abs(), crop.y.unsigned_abs());
        for (v, line) in (0..out.height).zip(bytes.chunks_exact_mut(stride as usize)) {
            for (u, px) in (0..out.width).zip(line.chunks_exact_mut(bpp as usize)) {
                let (sx, sy) = map.source(u, v);
                let (sx, sy) = (sx + cx, sy + cy);
                match layout.format {
                    CpuPixelFormat::Rgba8 => px.copy_from_slice(&self.rgba8(sx, sy)),
                    CpuPixelFormat::Bgra8 => {
                        let [r, g, b, a] = self.rgba8(sx, sy);
                        px.copy_from_slice(&[b, g, r, a]);
                    }
                    CpuPixelFormat::Rgba16f => px.copy_from_slice(&self.rgba16f(sx, sy)),
                }
            }
        }
        Ok(Pixels {
            size: out,
            stride,
            format: layout.format,
            bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_crop_is_refused_not_walked() {
        let bytes = pattern();
        let view = CpuView::new(&bytes, Size::new(3, 2), 12, PixelFormat::Bgrx8).expect("view");
        for crop in [
            Rect::new(0, 0, 0, 2),
            Rect::new(1, 1, 2, 0),
            Rect::new(0, 0, 0, 0),
        ] {
            assert!(
                matches!(
                    view.convert(crop, Orientation::UPRIGHT, PixelLayout::default()),
                    Err(PixelError::CropOutside { .. })
                ),
                "{crop:?}"
            );
        }
    }

    /// A 3×2 Bgrx8 image whose pixel (x, y) has red = 10·y + x.
    fn pattern() -> Vec<u8> {
        let mut bytes = Vec::new();
        for y in 0..2u8 {
            for x in 0..3u8 {
                bytes.extend_from_slice(&[0, 0, 10 * y + x, 0xee]);
            }
        }
        bytes
    }

    fn reds(pixels: &Pixels) -> Vec<u8> {
        pixels
            .bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|px| px[0])
            .collect()
    }

    fn convert(orientation: Orientation, crop: Rect) -> Pixels {
        let bytes = pattern();
        let view = CpuView::new(&bytes, Size::new(3, 2), 12, PixelFormat::Bgrx8).expect("view");
        view.convert(crop, orientation, PixelLayout::default())
            .expect("convert")
    }

    const FULL: Rect = Rect::new(0, 0, 3, 2);

    #[test]
    fn identity_orientation_keeps_frame_untouched() {
        let out = convert(Orientation::UPRIGHT, FULL);
        assert_eq!(out.size, Size::new(3, 2));
        assert_eq!(reds(&out), [0, 1, 2, 10, 11, 12]);
        assert_eq!(out.bytes[3], 255, "opaque formats read with full alpha");
    }

    #[test]
    fn rotation_by_90_swaps_axes() {
        let rotated = Orientation {
            mirrored: false,
            rotation: Rotation::R90,
        };
        let out = convert(rotated, FULL);
        assert_eq!(out.size, Size::new(2, 3));
        // Clockwise: the bottom-left pixel ends up top-left.
        assert_eq!(reds(&out), [10, 0, 11, 1, 12, 2]);
        let out = convert(
            Orientation {
                mirrored: false,
                rotation: Rotation::R270,
            },
            FULL,
        );
        assert_eq!(reds(&out), [2, 12, 1, 11, 0, 10]);
    }

    #[test]
    fn y_invert_flips_rows_and_drops_stride_padding() {
        let mut padded = Vec::new();
        for row in pattern().as_chunks::<12>().0 {
            padded.extend_from_slice(row);
            padded.extend_from_slice(&[0xff; 4]);
        }
        let view = CpuView::new(&padded, Size::new(3, 2), 16, PixelFormat::Bgrx8).expect("view");
        let out = view
            .convert(FULL, Orientation::Y_INVERT, PixelLayout::default())
            .expect("convert");
        assert_eq!(out.stride, 12);
        assert_eq!(reds(&out), [10, 11, 12, 0, 1, 2]);
    }

    #[test]
    fn crop_comes_before_orientation() {
        let out = convert(
            Orientation {
                mirrored: true,
                rotation: Rotation::R0,
            },
            Rect::new(1, 0, 2, 2),
        );
        assert_eq!(reds(&out), [2, 1, 12, 11]);
    }

    #[test]
    fn upright_false_keeps_buffer_order() {
        let bytes = pattern();
        let view = CpuView::new(&bytes, Size::new(3, 2), 12, PixelFormat::Bgrx8).expect("view");
        let layout = PixelLayout {
            upright: false,
            ..PixelLayout::default()
        };
        let out = view
            .convert(
                FULL,
                Orientation {
                    mirrored: false,
                    rotation: Rotation::R90,
                },
                layout,
            )
            .expect("convert");
        assert_eq!(reds(&out), [0, 1, 2, 10, 11, 12]);
    }

    #[test]
    fn bgra_output_swaps_back() {
        let bytes = pattern();
        let view = CpuView::new(&bytes, Size::new(3, 2), 12, PixelFormat::Bgrx8).expect("view");
        let layout = PixelLayout {
            format: CpuPixelFormat::Bgra8,
            ..PixelLayout::default()
        };
        let out = view
            .convert(FULL, Orientation::UPRIGHT, layout)
            .expect("convert");
        assert_eq!(&out.bytes[4..8], &[0, 0, 1, 255]);
    }

    #[test]
    fn untight_rows_are_padded() {
        let bytes = pattern();
        let view = CpuView::new(&bytes, Size::new(3, 2), 12, PixelFormat::Bgrx8).expect("view");
        let layout = PixelLayout {
            tight: false,
            ..PixelLayout::default()
        };
        let out = view
            .convert(FULL, Orientation::UPRIGHT, layout)
            .expect("convert");
        assert_eq!(out.stride, 64);
        assert_eq!(out.bytes.len(), 128);
    }

    #[test]
    fn ten_bit_reads_round_to_eight() {
        let w: u32 = (1023 << 20) | (512 << 10);
        let bytes = w.to_le_bytes();
        let view = CpuView::new(&bytes, Size::new(1, 1), 4, PixelFormat::Xr30).expect("view");
        assert_eq!(view.rgba8(0, 0), [255, 128, 0, 255]);
    }

    #[test]
    fn never_tone_maps() {
        let bytes = [0u8; 8];
        let view = CpuView::new(&bytes, Size::new(1, 1), 8, PixelFormat::Rgba16f).expect("view");
        let err = view
            .convert(
                Rect::new(0, 0, 1, 1),
                Orientation::UPRIGHT,
                PixelLayout::default(),
            )
            .expect_err("hdr to sdr");
        assert!(matches!(err, PixelError::ToneMapping { .. }));
    }

    #[test]
    fn short_buffers_and_bad_crops_are_refused() {
        assert!(matches!(
            CpuView::new(&[0u8; 10], Size::new(3, 1), 12, PixelFormat::Bgrx8),
            Err(PixelError::ShortBuffer { .. })
        ));
        let bytes = pattern();
        let view = CpuView::new(&bytes, Size::new(3, 2), 12, PixelFormat::Bgrx8).expect("view");
        assert!(matches!(
            view.convert(
                Rect::new(2, 0, 2, 2),
                Orientation::UPRIGHT,
                PixelLayout::default()
            ),
            Err(PixelError::CropOutside { .. })
        ));
    }

    #[test]
    fn buffer_rects_follow_the_orientation() {
        let map = OrientationMap {
            size: Size::new(4, 2),
            orientation: Orientation {
                mirrored: false,
                rotation: Rotation::R90,
            },
        };
        // Upright is 2x4; its top row (y = 0) is the buffer's left column.
        assert_eq!(
            map.buffer_rect(Rect::new(0, 0, 2, 1)),
            Rect::new(0, 0, 1, 2)
        );
        assert_eq!(
            map.buffer_rect(Rect::new(0, 3, 2, 1)),
            Rect::new(3, 0, 1, 2)
        );
    }

    #[test]
    fn orientation_composes_like_the_pixels() {
        // Applying a then b equals applying a.then(b) in one step.
        let all: Vec<Orientation> = [false, true]
            .into_iter()
            .flat_map(|mirrored| {
                [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270]
                    .into_iter()
                    .map(move |rotation| Orientation { mirrored, rotation })
            })
            .collect();
        for a in &all {
            for b in &all {
                let once = convert(a.then(*b), FULL);
                let first = convert(*a, FULL);
                let view = CpuView::new(&first.bytes, first.size, first.stride, PixelFormat::Rgbx8)
                    .expect("view");
                let twice = view
                    .convert(Rect::from_size(first.size), *b, PixelLayout::default())
                    .expect("convert");
                assert_eq!(reds(&once), reds(&twice), "{a:?} then {b:?}");
            }
        }
    }
}
