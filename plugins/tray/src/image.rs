//! Icon decoding: every representation is checked when the icon is set,
//! and rasters are produced per size on demand and cached.
//!
//! Nothing here draws content: SVG is rasterised as given, PNG / JPEG
//! are only scaled to the size a host asks for.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

use istmo_core::sync::lock;
use istmo_menu::{IconImage, ImageData};

use crate::{ImageError, ImageFailure, TrayError};

/// Largest encoded representation accepted.
pub(crate) const MAX_IMAGE_BYTES: usize = 8 << 20;
/// Largest decoded width or height kept.
pub(crate) const MAX_DIMENSION: u32 = 1024;
/// Most frames an animation may have.
pub(crate) const MAX_FRAMES: usize = 512;

/// Pixels in straight (not premultiplied) RGBA, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Raster {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

impl Raster {
    /// The pixels as a PNG file.
    pub(crate) fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        // Writing into a Vec only fails on a size mismatch, which the
        // constructor rules out.
        let written = encoder
            .write_header()
            .and_then(|mut writer| writer.write_image_data(&self.rgba));
        if let Err(err) = written {
            tracing::warn!(%err, "tray: PNG encoding failed");
            out.clear();
        }
        out
    }

    /// The pixels fitted (aspect kept) and centred in a `size` × `size`
    /// square with transparent margins.
    fn fit_square(&self, size: u32) -> Self {
        if self.width == size && self.height == size {
            return self.clone();
        }
        let longest = self.width.max(self.height);
        let scaled_w = scaled(self.width, size, longest);
        let scaled_h = scaled(self.height, size, longest);
        let inner = resample(self, scaled_w, scaled_h);
        let mut out = Self {
            width: size,
            height: size,
            rgba: vec![0; (size as usize) * (size as usize) * 4],
        };
        let left = (size - scaled_w) / 2;
        let top = (size - scaled_h) / 2;
        let row_bytes = scaled_w as usize * 4;
        for (row, line) in inner.rgba.chunks_exact(row_bytes).enumerate() {
            let start = ((top as usize + row) * size as usize + left as usize) * 4;
            out.rgba[start..start + row_bytes].copy_from_slice(line);
        }
        out
    }
}

/// `length` scaled by `target / longest`, at least one pixel.
fn scaled(length: u32, target: u32, longest: u32) -> u32 {
    let value = u64::from(length) * u64::from(target) / u64::from(longest.max(1));
    u32::try_from(value).unwrap_or(target).clamp(1, target)
}

/// Area-averaged resampling in premultiplied space (no dark fringes
/// around transparent edges).
fn resample(src: &Raster, width: u32, height: u32) -> Raster {
    let (sw, sh) = (f64::from(src.width), f64::from(src.height));
    let (dw, dh) = (f64::from(width), f64::from(height));
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        let y0 = f64::from(y) * sh / dh;
        let y1 = (f64::from(y + 1) * sh / dh).max(y0 + 1e-9);
        for x in 0..width {
            let x0 = f64::from(x) * sw / dw;
            let x1 = (f64::from(x + 1) * sw / dw).max(x0 + 1e-9);
            rgba.extend_from_slice(&average(src, x0, x1, y0, y1));
        }
    }
    Raster {
        width,
        height,
        rgba,
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "coordinates are within the source and channel sums within 0..=255 after division"
)]
fn average(src: &Raster, x0: f64, x1: f64, y0: f64, y1: f64) -> [u8; 4] {
    let mut sum = [0.0f64; 4];
    let mut total = 0.0;
    let last_x = src.width.saturating_sub(1);
    let last_y = src.height.saturating_sub(1);
    let mut sy = y0.floor() as u32;
    while f64::from(sy) < y1 && sy <= last_y {
        let wy = (f64::from(sy + 1).min(y1) - f64::from(sy).max(y0)).max(0.0);
        let mut sx = x0.floor() as u32;
        while f64::from(sx) < x1 && sx <= last_x {
            let wx = (f64::from(sx + 1).min(x1) - f64::from(sx).max(x0)).max(0.0);
            let weight = wx * wy;
            let at = ((sy * src.width + sx) * 4) as usize;
            let alpha = f64::from(src.rgba[at + 3]) / 255.0;
            for (channel, value) in sum.iter_mut().take(3).zip(&src.rgba[at..at + 3]) {
                *channel += f64::from(*value) * alpha * weight;
            }
            sum[3] += alpha * weight;
            total += weight;
            sx += 1;
        }
        sy += 1;
    }
    if total <= 0.0 || sum[3] <= 0.0 {
        return [0; 4];
    }
    let alpha = sum[3] / total;
    let channel = |value: f64| (value / sum[3]).round().clamp(0.0, 255.0) as u8;
    [
        channel(sum[0]),
        channel(sum[1]),
        channel(sum[2]),
        (alpha * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

/// One decoded representation.
enum Rep {
    #[cfg(feature = "svg")]
    Svg(Box<resvg::usvg::Tree>),
    Raster(Raster),
    Name(String),
}

impl std::fmt::Debug for Rep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(feature = "svg")]
            Self::Svg(_) => f.write_str("Svg"),
            Self::Raster(r) => write!(f, "Raster({}x{})", r.width, r.height),
            Self::Name(name) => write!(f, "Name({name})"),
        }
    }
}

/// A decoded [`IconImage`]: checked once, rasterised per size on demand.
#[derive(Debug)]
pub(crate) struct DecodedIcon {
    reps: Vec<Rep>,
    template: bool,
    cache: Mutex<HashMap<u32, Arc<Raster>>>,
}

impl DecodedIcon {
    /// Decodes every representation of `image`. `names` tells whether
    /// the backend shows platform icon names; `frame` goes into errors.
    pub(crate) fn decode(image: &IconImage, frame: u32, names: bool) -> Result<Self, TrayError> {
        let mut reps = Vec::with_capacity(image.reps.len());
        for (at, data) in image.reps.iter().enumerate() {
            let fail = |reason| {
                TrayError::Image(ImageError {
                    frame,
                    rep: u32::try_from(at).unwrap_or(u32::MAX),
                    reason,
                })
            };
            let rep = match data {
                ImageData::PlatformName(name) if names => Rep::Name(name.clone()),
                ImageData::Svg(bytes) => decode_svg(bytes).map_err(fail)?,
                ImageData::Png(bytes) => Rep::Raster(decode_png(bytes).map_err(fail)?),
                ImageData::Jpeg(bytes) => Rep::Raster(decode_jpeg(bytes).map_err(fail)?),
                // Names where the backend has none, and formats added to
                // the model later.
                _ => continue,
            };
            reps.push(rep);
        }
        if reps.is_empty() {
            return Err(TrayError::Image(ImageError {
                frame,
                rep: 0,
                reason: ImageFailure::NothingUsable,
            }));
        }
        Ok(Self {
            reps,
            template: image.template,
            cache: Mutex::new(HashMap::new()),
        })
    }

    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "template images exist only on macOS")
    )]
    pub(crate) const fn is_template(&self) -> bool {
        self.template
    }

    /// The first platform icon name listed, when the backend uses names.
    pub(crate) fn name(&self) -> Option<&str> {
        self.reps.iter().find_map(|rep| match rep {
            Rep::Name(name) => Some(name.as_str()),
            #[cfg(feature = "svg")]
            Rep::Svg(_) => None,
            Rep::Raster(_) => None,
        })
    }

    /// The icon as a `size` × `size` square. `None` when it only has a
    /// platform name.
    pub(crate) fn raster(&self, size: u32) -> Option<Arc<Raster>> {
        let size = size.clamp(1, MAX_DIMENSION);
        if let Some(hit) = lock(&self.cache).get(&size) {
            return Some(Arc::clone(hit));
        }
        let raster = Arc::new(self.render(size)?);
        lock(&self.cache).insert(size, Arc::clone(&raster));
        Some(raster)
    }

    fn render(&self, size: u32) -> Option<Raster> {
        #[cfg(feature = "svg")]
        if let Some(tree) = self.reps.iter().find_map(|rep| match rep {
            Rep::Svg(tree) => Some(tree),
            Rep::Raster(_) | Rep::Name(_) => None,
        }) {
            return Some(render_svg(tree, size));
        }
        // The smallest raster at least `size` large, else the largest.
        let rasters = self.reps.iter().filter_map(|rep| match rep {
            Rep::Raster(raster) => Some(raster),
            #[cfg(feature = "svg")]
            Rep::Svg(_) => None,
            Rep::Name(_) => None,
        });
        let longest = |r: &&Raster| r.width.max(r.height);
        let (large, small): (Vec<&Raster>, Vec<&Raster>) =
            rasters.partition(|r| longest(r) >= size);
        let chosen = large
            .into_iter()
            .min_by_key(longest)
            .or_else(|| small.into_iter().max_by_key(longest))?;
        Some(chosen.fit_square(size))
    }
}

fn check_size(bytes: &[u8]) -> Result<(), ImageFailure> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(ImageFailure::Decode(format!(
            "{} bytes, more than {MAX_IMAGE_BYTES}",
            bytes.len()
        )));
    }
    if bytes.is_empty() {
        return Err(ImageFailure::Empty);
    }
    Ok(())
}

fn check_dimensions(width: u32, height: u32) -> Result<(), ImageFailure> {
    if width == 0 || height == 0 {
        return Err(ImageFailure::Empty);
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(ImageFailure::Decode(format!(
            "{width} x {height}, larger than {MAX_DIMENSION} x {MAX_DIMENSION}"
        )));
    }
    Ok(())
}

#[cfg(feature = "svg")]
fn decode_svg(bytes: &[u8]) -> Result<Rep, ImageFailure> {
    check_size(bytes)?;
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default())
        .map_err(|err| ImageFailure::Decode(err.to_string()))?;
    let size = tree.size();
    if size.width() <= 0.0 || size.height() <= 0.0 {
        return Err(ImageFailure::Empty);
    }
    Ok(Rep::Svg(Box::new(tree)))
}

#[cfg(not(feature = "svg"))]
const fn decode_svg(_bytes: &[u8]) -> Result<Rep, ImageFailure> {
    Err(ImageFailure::FormatNotCompiled)
}

#[cfg(feature = "svg")]
fn render_svg(tree: &resvg::usvg::Tree, size: u32) -> Raster {
    use resvg::tiny_skia::{Pixmap, Transform};

    let view = tree.size();
    let scale = f64::from(size) / f64::from(view.width().max(view.height()));
    #[expect(
        clippy::cast_possible_truncation,
        reason = "scale and offsets are within the icon size"
    )]
    let (scale, dx, dy) = (
        scale as f32,
        (f64::from(view.width()).mul_add(-scale, f64::from(size)) / 2.0) as f32,
        (f64::from(view.height()).mul_add(-scale, f64::from(size)) / 2.0) as f32,
    );
    let Some(mut pixmap) = Pixmap::new(size, size) else {
        return Raster {
            width: size,
            height: size,
            rgba: vec![0; size as usize * size as usize * 4],
        };
    };
    let transform = Transform::from_scale(scale, scale).post_translate(dx, dy);
    resvg::render(tree, transform, &mut pixmap.as_mut());
    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    Raster {
        width: size,
        height: size,
        rgba,
    }
}

fn decode_png(bytes: &[u8]) -> Result<Raster, ImageFailure> {
    check_size(bytes)?;
    let decode_err = |err: png::DecodingError| ImageFailure::Decode(err.to_string());
    let limits = png::Limits {
        bytes: (MAX_DIMENSION as usize) * (MAX_DIMENSION as usize) * 8,
    };
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(bytes), limits);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(decode_err)?;
    let (width, height) = {
        let info = reader.info();
        (info.width, info.height)
    };
    check_dimensions(width, height)?;
    let len = reader
        .output_buffer_size()
        .ok_or_else(|| ImageFailure::Decode("image too large".to_owned()))?;
    let mut buf = vec![0; len];
    let frame = reader.next_frame(&mut buf).map_err(decode_err)?;
    buf.truncate(frame.buffer_size());
    let rgba = match frame.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        png::ColorType::Indexed => {
            return Err(ImageFailure::Decode("palette not expanded".to_owned()));
        }
    };
    Ok(Raster {
        width,
        height,
        rgba,
    })
}

#[cfg(feature = "jpeg")]
fn decode_jpeg(bytes: &[u8]) -> Result<Raster, ImageFailure> {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    check_size(bytes)?;
    let options = DecoderOptions::default()
        .set_max_width(MAX_DIMENSION as usize)
        .set_max_height(MAX_DIMENSION as usize)
        .jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    let rgba = decoder
        .decode()
        .map_err(|err| ImageFailure::Decode(format!("{err:?}")))?;
    let info = decoder
        .info()
        .ok_or_else(|| ImageFailure::Decode("no image info".to_owned()))?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    check_dimensions(width, height)?;
    if rgba.len() != width as usize * height as usize * 4 {
        return Err(ImageFailure::Decode("unexpected pixel layout".to_owned()));
    }
    Ok(Raster {
        width,
        height,
        rgba,
    })
}

#[cfg(not(feature = "jpeg"))]
const fn decode_jpeg(_bytes: &[u8]) -> Result<Raster, ImageFailure> {
    Err(ImageFailure::FormatNotCompiled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, pixel: [u8; 4]) -> Raster {
        Raster {
            width,
            height,
            rgba: pixel.repeat((width * height) as usize),
        }
    }

    #[test]
    fn png_round_trip() {
        let raster = solid(3, 2, [10, 20, 30, 200]);
        let decoded = decode_png(&raster.to_png()).unwrap();
        assert_eq!(decoded, raster);
    }

    #[test]
    fn fit_square_keeps_aspect_and_centres() {
        let raster = solid(4, 2, [255, 0, 0, 255]).fit_square(4);
        assert_eq!((raster.width, raster.height), (4, 4));
        // Rows 0 and 3 are margin, rows 1 and 2 the image.
        assert_eq!(&raster.rgba[0..4], &[0, 0, 0, 0]);
        assert_eq!(&raster.rgba[16..20], &[255, 0, 0, 255]);
        assert_eq!(&raster.rgba[48..52], &[0, 0, 0, 0]);
    }

    #[test]
    fn downscale_averages_without_dark_fringes() {
        let mut raster = solid(2, 1, [255, 255, 255, 255]);
        raster.rgba[4..8].copy_from_slice(&[0, 0, 0, 0]);
        let scaled = resample(&raster, 1, 1);
        assert_eq!(scaled.rgba, [255, 255, 255, 128]);
    }

    #[test]
    fn picks_the_smallest_raster_not_smaller_than_the_target() {
        let image = IconImage {
            reps: vec![
                ImageData::Png(solid(16, 16, [1, 1, 1, 255]).to_png()),
                ImageData::Png(solid(64, 64, [2, 2, 2, 255]).to_png()),
                ImageData::Png(solid(32, 32, [3, 3, 3, 255]).to_png()),
            ],
            template: false,
        };
        let icon = DecodedIcon::decode(&image, 0, false).unwrap();
        assert_eq!(&icon.raster(24).unwrap().rgba[0..4], &[3, 3, 3, 255]);
        assert_eq!(&icon.raster(128).unwrap().rgba[0..4], &[2, 2, 2, 255]);
    }

    #[test]
    fn names_are_skipped_where_unsupported() {
        let image = IconImage {
            reps: vec![ImageData::PlatformName("bell".to_owned())],
            template: false,
        };
        let err = DecodedIcon::decode(&image, 2, false).unwrap_err();
        assert_eq!(
            err,
            TrayError::Image(ImageError {
                frame: 2,
                rep: 0,
                reason: ImageFailure::NothingUsable,
            })
        );
        let named = DecodedIcon::decode(&image, 0, true).unwrap();
        assert_eq!(named.name(), Some("bell"));
        assert!(named.raster(16).is_none());
    }

    #[test]
    fn garbage_is_a_decode_error() {
        let image = IconImage::new(ImageData::Png(vec![1, 2, 3]));
        assert!(matches!(
            DecodedIcon::decode(&image, 0, false),
            Err(TrayError::Image(ImageError {
                reason: ImageFailure::Decode(_),
                ..
            }))
        ));
    }

    #[cfg(feature = "svg")]
    #[test]
    fn svg_renders_at_the_asked_size() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="#ff0000"/></svg>"##;
        let icon =
            DecodedIcon::decode(&IconImage::new(ImageData::Svg(svg.to_vec())), 0, false).unwrap();
        let raster = icon.raster(22).unwrap();
        assert_eq!((raster.width, raster.height), (22, 22));
        assert_eq!(&raster.rgba[0..4], &[255, 0, 0, 255]);
    }

    #[cfg(feature = "jpeg")]
    #[test]
    fn jpeg_garbage_is_refused() {
        assert!(decode_jpeg(&[0xff, 0xd8, 0x00]).is_err());
    }
}
