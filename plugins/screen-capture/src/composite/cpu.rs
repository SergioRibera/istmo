//! CPU composite engine: row-by-row nearest-neighbour blits into one
//! `Bgra8` buffer.

use crate::pixels::{CpuView, OrientationMap};
use crate::types::{CompositeFill, Orientation, Rect, Size};

use super::{CompositeLayout, logical_to_buffer};

/// One captured part, ready to blit.
#[derive(Debug, Clone, Copy)]
pub struct Part<'a> {
    pub view: CpuView<'a>,
    /// What turns the buffer upright.
    pub orientation: Orientation,
}

/// A composed `Bgra8` image, rows `width × 4` bytes apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composed {
    pub size: Size,
    pub bytes: Vec<u8>,
}

/// Compose `parts` (in `layout.parts` order) into one `Bgra8` buffer.
/// Gaps are transparent or black per `fill`.
///
/// # Panics
/// Never: a part list shorter than the layout leaves the missing parts
/// filled.
#[must_use]
pub fn compose(layout: &CompositeLayout, parts: &[Part<'_>], fill: CompositeFill) -> Composed {
    let size = layout.size;
    let stride = size.width as usize * 4;
    let fill_px: [u8; 4] = match fill {
        CompositeFill::Transparent => [0, 0, 0, 0],
        CompositeFill::Black => [0, 0, 0, 255],
    };
    let mut bytes = fill_px.repeat(size.width as usize * size.height as usize);
    for (plan, part) in layout.parts.iter().zip(parts) {
        let map = OrientationMap {
            size: part.view.size(),
            orientation: part.orientation,
        };
        let upright = map.output_size();
        let src = logical_to_buffer(plan.visible, plan.scale_permille, upright);
        let dest = plan.dest;
        if src.is_empty() || dest.is_empty() {
            continue;
        }
        blit(&mut bytes, stride, size, dest, src, &map, &part.view);
    }
    Composed { size, bytes }
}

#[expect(
    clippy::many_single_char_names,
    reason = "x, y, u, v, dx, dy are the usual pixel coordinate names"
)]
fn blit(
    out: &mut [u8],
    stride: usize,
    size: Size,
    dest: Rect,
    src: Rect,
    map: &OrientationMap,
    view: &CpuView<'_>,
) {
    let (sx0, sy0) = (src.x.unsigned_abs(), src.y.unsigned_abs());
    for dy in 0..dest.height {
        let y = i64::from(dest.y) + i64::from(dy);
        if y < 0 || y >= i64::from(size.height) {
            continue;
        }
        // Nearest source row (centre sampling).
        let v = u64::from(sy0)
            + ((u64::from(dy) * 2 + 1) * u64::from(src.height) / (u64::from(dest.height) * 2));
        let v = u32::try_from(v).unwrap_or(u32::MAX);
        let row_start = usize::try_from(y).unwrap_or(0) * stride;
        for dx in 0..dest.width {
            let x = i64::from(dest.x) + i64::from(dx);
            if x < 0 || x >= i64::from(size.width) {
                continue;
            }
            let u = u64::from(sx0)
                + ((u64::from(dx) * 2 + 1) * u64::from(src.width) / (u64::from(dest.width) * 2));
            let u = u32::try_from(u).unwrap_or(u32::MAX);
            let (bx, by) = map.source(u, v);
            let [r, g, b, a] = view.rgba8(bx, by);
            let at = row_start + usize::try_from(x).unwrap_or(0) * 4;
            if let Some(px) = out.get_mut(at..at + 4) {
                px.copy_from_slice(&[b, g, r, a]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composite::layout;
    use crate::types::{
        BackendId, CompositeConfig, CompositeScale, PixelFormat, Rotation, Source, SourceId,
        SourceKind,
    };

    fn solid(width: u32, height: u32, red: u8) -> Vec<u8> {
        [0, 0, red, 255].repeat((width * height) as usize)
    }

    fn monitor(name: &str, geometry: Rect, scale: u32) -> Source {
        let mut source = Source::new(
            SourceId::new(BackendId::LinuxX11, name),
            SourceKind::Monitor,
            name,
        );
        source.geometry = Some(geometry);
        source.scale_permille = Some(scale);
        source
    }

    #[test]
    fn parts_land_side_by_side_and_gaps_stay_transparent() {
        let sources = [
            monitor("a", Rect::new(0, 0, 2, 2), 1000),
            monitor("b", Rect::new(2, 0, 2, 1), 1000),
        ];
        let layout = layout(&sources, None, &CompositeConfig::default()).expect("layout");
        let (a, b) = (solid(2, 2, 10), solid(2, 1, 20));
        let parts = [
            Part {
                view: CpuView::new(&a, Size::new(2, 2), 8, PixelFormat::Bgra8).expect("a"),
                orientation: Orientation::UPRIGHT,
            },
            Part {
                view: CpuView::new(&b, Size::new(2, 1), 8, PixelFormat::Bgra8).expect("b"),
                orientation: Orientation::UPRIGHT,
            },
        ];
        let out = compose(&layout, &parts, CompositeFill::Transparent);
        assert_eq!(out.size, Size::new(4, 2));
        let red: Vec<u8> = out.bytes.as_chunks::<4>().0.iter().map(|p| p[2]).collect();
        assert_eq!(red, [10, 10, 20, 20, 10, 10, 0, 0]);
        assert_eq!(out.bytes[4 * 7 + 3], 0, "the gap is transparent");
    }

    #[test]
    fn lower_scale_parts_are_upsampled_and_rotated_parts_turned() {
        let sources = [
            monitor("hi", Rect::new(0, 0, 1, 1), 2000),
            monitor("lo", Rect::new(1, 0, 1, 1), 1000),
        ];
        let layout = layout(
            &sources,
            None,
            &CompositeConfig {
                scale: CompositeScale::HighestScale,
                ..CompositeConfig::default()
            },
        )
        .expect("layout");
        assert_eq!(layout.size, Size::new(4, 2));
        // A 2×2 hi-dpi buffer rotated by 90°: upright pixel (0, 0) is
        // buffer (0, 1).
        let mut hi = solid(2, 2, 1);
        hi[4 * 2 + 2] = 99;
        let lo = solid(1, 1, 50);
        let parts = [
            Part {
                view: CpuView::new(&hi, Size::new(2, 2), 8, PixelFormat::Bgra8).expect("hi"),
                orientation: Orientation {
                    mirrored: false,
                    rotation: Rotation::R90,
                },
            },
            Part {
                view: CpuView::new(&lo, Size::new(1, 1), 4, PixelFormat::Bgra8).expect("lo"),
                orientation: Orientation::UPRIGHT,
            },
        ];
        let out = compose(&layout, &parts, CompositeFill::Black);
        let red: Vec<u8> = out.bytes.as_chunks::<4>().0.iter().map(|p| p[2]).collect();
        assert_eq!(red, [99, 1, 50, 50, 1, 1, 50, 50]);
    }
}
