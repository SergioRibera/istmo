//! Multi-monitor composite: layout maths and the CPU engine.
//!
//! One capture per monitor, all on the same backend; each part is
//! blitted upright at the composite scale into a plugin-owned buffer.
//! `HighestScale` renders at the largest monitor scale so no monitor is
//! downsampled. Either engine is a declared copy
//! (`CopyStage::Composite`). Streaming composite (GPU) is not
//! implemented yet: backends report `composite.streaming = Unsupported`.

pub mod cpu;

use crate::types::{CompositeConfig, CompositeScale, Rect, Size, Source, SourceId};

/// Why a composite cannot be laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    /// A part has no known geometry.
    NoGeometry(SourceId),
    /// The region misses every part.
    Empty,
    /// The composite would not fit in memory.
    TooLarge,
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoGeometry(id) => write!(f, "source `{}` has no geometry", id.value),
            Self::Empty => f.write_str("the region covers no monitor"),
            Self::TooLarge => f.write_str("the composite is too large"),
        }
    }
}

impl std::error::Error for LayoutError {}

/// Where one part goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartLayout {
    pub source: SourceId,
    /// The visible part of the source, logical, relative to the
    /// source's own top-left.
    pub visible: Rect,
    /// Where it lands in the composite buffer (pixels).
    pub dest: Rect,
    pub scale_permille: u32,
}

/// A composite's layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositeLayout {
    pub size: Size,
    /// Composite pixels per logical pixel, in permille.
    pub scale_permille: u32,
    /// Global logical rectangle the composite covers.
    pub area: Rect,
    pub parts: Vec<PartLayout>,
}

fn scale_len(len: i64, permille: u32) -> i64 {
    // Rounded to nearest.
    (len * i64::from(permille) + 500).div_euclid(1000)
}

/// Lay out `sources` (monitors with geometry), cropped to `region`
/// (global logical) when given.
///
/// # Errors
/// A source without geometry, a region that misses every source, or a
/// composite too large to allocate.
pub fn layout(
    sources: &[Source],
    region: Option<Rect>,
    config: &CompositeConfig,
) -> Result<CompositeLayout, LayoutError> {
    let mut placed = Vec::with_capacity(sources.len());
    for source in sources {
        let geometry = source
            .geometry
            .ok_or_else(|| LayoutError::NoGeometry(source.id.clone()))?;
        let visible = match region {
            Some(region) => match geometry.intersect(region) {
                Some(visible) => visible,
                None => continue,
            },
            None => geometry,
        };
        placed.push((source, geometry, visible));
    }
    let area = placed
        .iter()
        .map(|(_, _, visible)| *visible)
        .reduce(Rect::union)
        .ok_or(LayoutError::Empty)?;
    let scale_permille = match config.scale {
        CompositeScale::HighestScale => placed
            .iter()
            .map(|(s, _, _)| s.scale())
            .max()
            .unwrap_or(1000),
        CompositeScale::Logical => 1000,
        CompositeScale::Permille(p) => p.max(1),
    };
    let to_px = |v: i64| scale_len(v, scale_permille);
    let width = u32::try_from(to_px(i64::from(area.width))).map_err(|_| LayoutError::TooLarge)?;
    let height = u32::try_from(to_px(i64::from(area.height))).map_err(|_| LayoutError::TooLarge)?;
    let parts = placed
        .into_iter()
        .map(|(source, geometry, visible)| {
            // Edges are scaled independently so neighbouring parts meet
            // without gaps or overlaps.
            let x0 = to_px(i64::from(visible.x) - i64::from(area.x));
            let y0 = to_px(i64::from(visible.y) - i64::from(area.y));
            let x1 = to_px(visible.right() - i64::from(area.x));
            let y1 = to_px(visible.bottom() - i64::from(area.y));
            let dest = Rect::new(
                i32::try_from(x0).map_err(|_| LayoutError::TooLarge)?,
                i32::try_from(y0).map_err(|_| LayoutError::TooLarge)?,
                u32::try_from(x1 - x0).map_err(|_| LayoutError::TooLarge)?,
                u32::try_from(y1 - y0).map_err(|_| LayoutError::TooLarge)?,
            );
            Ok(PartLayout {
                source: source.id.clone(),
                visible: visible.relative_to(geometry.x, geometry.y),
                dest,
                scale_permille: source.scale(),
            })
        })
        .collect::<Result<Vec<_>, LayoutError>>()?;
    Ok(CompositeLayout {
        size: Size::new(width, height),
        scale_permille,
        area,
        parts,
    })
}

/// The buffer rectangle (physical pixels of an upright source) showing
/// `visible` (logical, source-relative) of a source at `scale_permille`.
#[must_use]
pub fn logical_to_buffer(visible: Rect, scale_permille: u32, buffer: Size) -> Rect {
    let px = |v: i64| scale_len(v, scale_permille).max(0);
    let x0 = px(i64::from(visible.x)).min(i64::from(buffer.width));
    let y0 = px(i64::from(visible.y)).min(i64::from(buffer.height));
    let x1 = px(visible.right()).min(i64::from(buffer.width));
    let y1 = px(visible.bottom()).min(i64::from(buffer.height));
    Rect::new(
        i32::try_from(x0).unwrap_or(i32::MAX),
        i32::try_from(y0).unwrap_or(i32::MAX),
        u32::try_from(x1 - x0).unwrap_or(0),
        u32::try_from(y1 - y0).unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BackendId, SourceKind};

    fn monitor(name: &str, geometry: Rect, scale: u32) -> Source {
        let mut source = Source::new(
            SourceId::new(BackendId::LinuxWlrScreencopy, name),
            SourceKind::Monitor,
            name,
        );
        source.geometry = Some(geometry);
        source.scale_permille = Some(scale);
        source
    }

    fn pair() -> Vec<Source> {
        vec![
            monitor("left", Rect::new(0, 0, 1920, 1080), 1000),
            monitor("right", Rect::new(1920, 0, 1280, 720), 1500),
        ]
    }

    #[test]
    fn highest_scale_never_downsamples() {
        let layout = layout(&pair(), None, &CompositeConfig::default()).expect("layout");
        assert_eq!(layout.scale_permille, 1500);
        assert_eq!(layout.size, Size::new(4800, 1620));
        assert_eq!(layout.parts[0].dest, Rect::new(0, 0, 2880, 1620));
        assert_eq!(layout.parts[1].dest, Rect::new(2880, 0, 1920, 1080));
    }

    #[test]
    fn logical_scale_renders_at_one() {
        let config = CompositeConfig {
            scale: CompositeScale::Logical,
            ..CompositeConfig::default()
        };
        let layout = layout(&pair(), None, &config).expect("layout");
        assert_eq!(layout.size, Size::new(3200, 1080));
    }

    #[test]
    fn a_region_crops_every_part() {
        let config = CompositeConfig {
            scale: CompositeScale::Logical,
            ..CompositeConfig::default()
        };
        let region = Rect::new(1820, 100, 200, 100);
        let layout = layout(&pair(), Some(region), &config).expect("layout");
        assert_eq!(layout.area, region);
        assert_eq!(layout.parts[0].visible, Rect::new(1820, 100, 100, 100));
        assert_eq!(layout.parts[1].visible, Rect::new(0, 100, 100, 100));
        assert_eq!(layout.parts[1].dest, Rect::new(100, 0, 100, 100));
        let missed = layout_err(Rect::new(-500, -500, 10, 10));
        assert_eq!(missed, LayoutError::Empty);
    }

    fn layout_err(region: Rect) -> LayoutError {
        layout(&pair(), Some(region), &CompositeConfig::default()).expect_err("no overlap")
    }

    #[test]
    fn logical_rects_map_to_physical_pixels() {
        let rect = logical_to_buffer(Rect::new(10, 10, 100, 50), 1500, Size::new(1920, 1080));
        assert_eq!(rect, Rect::new(15, 15, 150, 75));
        let clamped =
            logical_to_buffer(Rect::new(1200, 700, 200, 200), 1500, Size::new(1920, 1080));
        assert_eq!(clamped, Rect::new(1800, 1050, 120, 30));
    }
}
