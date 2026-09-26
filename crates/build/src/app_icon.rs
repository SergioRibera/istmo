//! Launcher icons generated from the single `[app] icon` source image:
//! Android `mipmap-*dpi` PNGs and an iOS asset-catalog app icon set.

use std::fmt;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};

/// Resource name of the generated Android launcher icon
/// (`@mipmap/istmo_launcher`).
pub const ANDROID_ICON_RESOURCE: &str = "istmo_launcher";

/// Name of the generated iOS app icon set
/// (`ASSETCATALOG_COMPILER_APPICON_NAME`).
pub const IOS_APP_ICON_SET: &str = "IstmoAppIcon";

/// Legacy launcher icon edge per density bucket.
const ANDROID_DENSITIES: &[(&str, u32)] = &[
    ("mdpi", 48),
    ("hdpi", 72),
    ("xhdpi", 96),
    ("xxhdpi", 144),
    ("xxxhdpi", 192),
];

/// Xcode 14+ derives every size from one 1024 pt universal image.
const IOS_ICON_EDGE: u32 = 1024;

const IOS_ICON_FILE: &str = "AppIcon-1024.png";

#[derive(Debug)]
pub enum IconError {
    Read {
        path: PathBuf,
        source: image::ImageError,
    },
    Encode(image::ImageError),
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for IconError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(f, "reading app icon {}: {source}", path.display())
            }
            Self::Encode(source) => write!(f, "encoding app icon: {source}"),
            Self::Write { path, source } => write!(f, "writing {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for IconError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Encode(source) => Some(source),
            Self::Write { source, .. } => Some(source),
        }
    }
}

/// Decoded `[app] icon` source image.
#[derive(Debug)]
pub struct AppIcon {
    image: DynamicImage,
}

impl AppIcon {
    pub fn open(path: &Path) -> Result<Self, IconError> {
        let image = image::open(path).map_err(|source| IconError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self { image })
    }

    /// Write `mipmap-<density>/istmo_launcher.png` under `res_dir`.
    pub fn write_android_mipmaps(&self, res_dir: &Path) -> Result<(), IconError> {
        for (density, edge) in ANDROID_DENSITIES {
            let png = encode_png(&self.resized(*edge))?;
            let dest = res_dir
                .join(format!("mipmap-{density}"))
                .join(format!("{ANDROID_ICON_RESOURCE}.png"));
            write_bytes_if_changed(&dest, &png)?;
        }
        Ok(())
    }

    /// Write an `IstmoAppIcon.appiconset` (plus the catalog's root
    /// `Contents.json`) under `xcassets_dir`. The App Store rejects
    /// icons with an alpha channel, so transparency is flattened onto
    /// white.
    pub fn write_ios_app_icon_set(&self, xcassets_dir: &Path) -> Result<(), IconError> {
        let flattened = DynamicImage::ImageRgb8(flatten_on_white(&self.resized(IOS_ICON_EDGE)));
        let set_dir = xcassets_dir.join(format!("{IOS_APP_ICON_SET}.appiconset"));
        write_bytes_if_changed(&set_dir.join(IOS_ICON_FILE), &encode_png(&flattened)?)?;
        let set_contents = format!(
            "{{\n  \"images\" : [\n    {{\n      \"filename\" : \"{IOS_ICON_FILE}\",\n      \
             \"idiom\" : \"universal\",\n      \"platform\" : \"ios\",\n      \
             \"size\" : \"1024x1024\"\n    }}\n  ],\n  \"info\" : {{\n    \
             \"author\" : \"istmo\",\n    \"version\" : 1\n  }}\n}}\n"
        );
        write_bytes_if_changed(&set_dir.join("Contents.json"), set_contents.as_bytes())?;
        let catalog_contents =
            "{\n  \"info\" : {\n    \"author\" : \"istmo\",\n    \"version\" : 1\n  }\n}\n";
        write_bytes_if_changed(
            &xcassets_dir.join("Contents.json"),
            catalog_contents.as_bytes(),
        )
    }

    fn resized(&self, edge: u32) -> DynamicImage {
        self.image.resize_exact(edge, edge, FilterType::Lanczos3)
    }
}

fn flatten_on_white(image: &DynamicImage) -> RgbImage {
    let rgba = image.to_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let blend = |channel: u8| {
            let alpha = u16::from(a);
            let value = (u16::from(channel) * alpha + 255 * (255 - alpha)) / 255;
            u8::try_from(value).unwrap_or(u8::MAX)
        };
        Rgb([blend(r), blend(g), blend(b)])
    })
}

fn encode_png(image: &DynamicImage) -> Result<Vec<u8>, IconError> {
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .map_err(IconError::Encode)?;
    Ok(bytes)
}

fn write_bytes_if_changed(dest: &Path, contents: &[u8]) -> Result<(), IconError> {
    if fs::read(dest).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    let write = || -> std::io::Result<()> {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(dest, contents)
    };
    write().map_err(|source| IconError::Write {
        path: dest.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("istmo-icon-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn icon() -> AppIcon {
        AppIcon {
            image: DynamicImage::ImageRgba8(RgbaImage::from_pixel(64, 64, Rgba([255, 0, 0, 0]))),
        }
    }

    #[test]
    fn writes_every_android_density() {
        let res = temp_dir("android");
        icon().write_android_mipmaps(&res).unwrap();
        for (density, edge) in ANDROID_DENSITIES {
            let path = res
                .join(format!("mipmap-{density}"))
                .join("istmo_launcher.png");
            let decoded = image::open(&path).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (*edge, *edge));
        }
    }

    #[test]
    fn ios_icon_is_opaque_1024() {
        let assets = temp_dir("ios");
        icon().write_ios_app_icon_set(&assets).unwrap();
        let set = assets.join("IstmoAppIcon.appiconset");
        let decoded = image::open(set.join(IOS_ICON_FILE)).unwrap();
        assert_eq!(decoded.width(), 1024);
        assert!(!decoded.color().has_alpha());
        // Fully transparent red flattens to white.
        assert_eq!(decoded.to_rgb8().get_pixel(0, 0).0, [255, 255, 255]);
        let contents = fs::read_to_string(set.join("Contents.json")).unwrap();
        assert!(contents.contains("\"filename\" : \"AppIcon-1024.png\""));
        assert!(assets.join("Contents.json").is_file());
    }
}
