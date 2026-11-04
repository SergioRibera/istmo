//! Launcher icons generated from `[app.icon]`:
//!
//! - `image` — one square source image for the iOS asset-catalog app
//!   icon set and the legacy Android `mipmap-*dpi` PNGs.
//! - `desktop` — the desktop variant (window icon, Windows `.exe` icon);
//!   defaults to `image`. Desktops never mask icons, so a full-bleed
//!   mobile source usually wants a shaped sibling here.
//! - `[app.icon.android]` — the layers of an Android adaptive icon
//!   (API 26+), written next to the legacy PNGs under the same
//!   `istmo_launcher` resource name so it takes over on every device
//!   that supports it.

use std::fmt;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::codecs::ico::{IcoEncoder, IcoFrame};
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, ImageFormat, Rgb, RgbImage};
use toml_edit::Item;

use crate::manifest::{ManifestError, expect_string, expect_table};

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

/// Adaptive layers are 108 dp square against the 48 dp legacy icon.
const ADAPTIVE_EDGE_NUMERATOR: u32 = 9;
const ADAPTIVE_EDGE_DENOMINATOR: u32 = 4;

/// Colour resource holding a solid adaptive icon background.
const ANDROID_BACKGROUND_COLOR_FILE: &str = "values/istmo_launcher_background.xml";

/// Frames of the Windows `.ico`: shell list views, title bars,
/// taskbar, Start, Explorer large icons.
const WINDOWS_ICO_EDGES: &[u32] = &[16, 24, 32, 48, 64, 256];

/// Xcode 14+ derives every size from one 1024 pt universal image.
const IOS_ICON_EDGE: u32 = 1024;

const IOS_ICON_FILE: &str = "AppIcon-1024.png";

#[derive(Debug)]
pub enum IconError {
    Read {
        path: PathBuf,
        source: image::ImageError,
    },
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
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
            Self::ReadFile { path, source } => {
                write!(f, "reading icon layer {}: {source}", path.display())
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
            Self::ReadFile { source, .. } | Self::Write { source, .. } => Some(source),
        }
    }
}

/// Decoded `[app.icon] image` (or a PNG adaptive layer).
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
        self.write_mipmaps(res_dir, ANDROID_ICON_RESOURCE, |edge| edge)
    }

    /// Write `mipmap-<density>/<name>.png` under `res_dir`, sized by
    /// `edge_of(legacy launcher edge)`.
    fn write_mipmaps(
        &self,
        res_dir: &Path,
        name: &str,
        edge_of: impl Fn(u32) -> u32,
    ) -> Result<(), IconError> {
        for (density, edge) in ANDROID_DENSITIES {
            let png = encode_png(&self.resized(edge_of(*edge)))?;
            let dest = res_dir
                .join(format!("mipmap-{density}"))
                .join(format!("{name}.png"));
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

    /// Write a square PNG of `edge` pixels, keeping transparency.
    pub fn write_png(&self, dest: &Path, edge: u32) -> Result<(), IconError> {
        write_bytes_if_changed(dest, &encode_png(&self.resized(edge))?)
    }

    /// Write a multi-resolution Windows `.ico` (PNG-compressed frames).
    pub fn write_ico(&self, dest: &Path) -> Result<(), IconError> {
        let frames = WINDOWS_ICO_EDGES
            .iter()
            .map(|edge| {
                let rgba = self.resized(*edge).to_rgba8();
                IcoFrame::as_png(rgba.as_raw(), *edge, *edge, ExtendedColorType::Rgba8)
                    .map_err(IconError::Encode)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut bytes = Vec::new();
        IcoEncoder::new(&mut bytes)
            .encode_images(&frames)
            .map_err(IconError::Encode)?;
        write_bytes_if_changed(dest, &bytes)
    }

    fn resized(&self, edge: u32) -> DynamicImage {
        self.image.resize_exact(edge, edge, FilterType::Lanczos3)
    }
}

/// `[app.icon]`: launcher icon sources, relative to the crate root until
/// [`IconConfig::rooted`] anchors them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IconConfig {
    /// Square source image for the iOS app icon and the legacy Android
    /// mipmaps.
    pub image: Option<PathBuf>,
    /// Desktop source image (window icon, Windows `.exe` icon); falls
    /// back to `image`.
    pub desktop: Option<PathBuf>,
    /// `[app.icon.android]`: Android adaptive icon layers.
    pub android: Option<AdaptiveIconConfig>,
}

/// `[app.icon.android]`: the layers of an Android adaptive icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdaptiveIconConfig {
    pub foreground: IconLayer,
    pub background: IconBackground,
    /// Single-colour layer for themed icons (Android 13+).
    pub monochrome: Option<IconLayer>,
}

/// One adaptive icon layer, by file type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IconLayer {
    /// PNG image, resized to every density bucket (108 dp: 432 px at
    /// xxxhdpi).
    Png(PathBuf),
    /// Android vector drawable, copied verbatim.
    Vector(PathBuf),
}

/// Background of an adaptive icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IconBackground {
    Color(IconColor),
    Layer(IconLayer),
}

/// `#RRGGBB` or `#AARRGGBB`, as Android colour resources spell it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconColor(String);

/// Adaptive icon layer slot; each slot owns one resource name.
#[derive(Debug, Clone, Copy)]
enum LayerSlot {
    Foreground,
    Background,
    Monochrome,
}

const KNOWN_ICON_KEYS: &[&str] = &["image", "desktop", "android"];
const KNOWN_ADAPTIVE_ICON_KEYS: &[&str] = &["foreground", "background", "monochrome"];

impl IconConfig {
    /// Parse the `[app.icon]` table.
    pub(crate) fn parse(item: &Item) -> Result<Self, ManifestError> {
        let table = item
            .as_table_like()
            .ok_or_else(|| ManifestError::TypeMismatch {
                key: "app.icon".to_owned(),
                expected: "table ([app.icon] with `image` / `desktop` and/or [app.icon.android])",
            })?;
        for (name, _) in table.iter() {
            if !KNOWN_ICON_KEYS.contains(&name) {
                return Err(ManifestError::UnknownKey {
                    key: format!("app.icon.{name}"),
                });
            }
        }
        Ok(Self {
            image: table
                .get("image")
                .map(|item| expect_string(item, "app.icon.image").map(PathBuf::from))
                .transpose()?,
            desktop: table
                .get("desktop")
                .map(|item| expect_string(item, "app.icon.desktop").map(PathBuf::from))
                .transpose()?,
            android: table
                .get("android")
                .map(AdaptiveIconConfig::parse)
                .transpose()?,
        })
    }

    /// Resolve every source path against `root`.
    #[must_use]
    pub fn rooted(&self, root: &Path) -> Self {
        Self {
            image: self.image.as_ref().map(|image| root.join(image)),
            desktop: self.desktop.as_ref().map(|desktop| root.join(desktop)),
            android: self.android.as_ref().map(|android| android.rooted(root)),
        }
    }

    /// Whether Android gets a launcher icon (`@mipmap/istmo_launcher`).
    #[must_use]
    pub const fn has_android_launcher(&self) -> bool {
        self.image.is_some() || self.android.is_some()
    }

    /// Source image for desktop targets: `desktop`, else `image`.
    #[must_use]
    pub fn desktop_source(&self) -> Option<&Path> {
        self.desktop.as_deref().or(self.image.as_deref())
    }

    /// Every source file, for `cargo:rerun-if-changed`.
    pub fn source_files(&self) -> impl Iterator<Item = &Path> {
        let android = self.android.iter().flat_map(|android| {
            let background = match &android.background {
                IconBackground::Color(_) => None,
                IconBackground::Layer(layer) => Some(layer),
            };
            std::iter::once(&android.foreground)
                .chain(background)
                .chain(android.monochrome.as_ref())
                .map(IconLayer::path)
        });
        self.image
            .as_deref()
            .into_iter()
            .chain(self.desktop.as_deref())
            .chain(android)
    }
}

impl AdaptiveIconConfig {
    fn parse(item: &Item) -> Result<Self, ManifestError> {
        let table = expect_table(item, "app.icon.android")?;
        for (name, _) in table.iter() {
            if !KNOWN_ADAPTIVE_ICON_KEYS.contains(&name) {
                return Err(ManifestError::UnknownKey {
                    key: format!("app.icon.android.{name}"),
                });
            }
        }
        let required = |key: &'static str, full: &'static str| {
            table
                .get(key)
                .ok_or(ManifestError::Missing { key: full })
                .and_then(|item| expect_string(item, full))
        };
        let background = required("background", "app.icon.android.background")?;
        Ok(Self {
            foreground: IconLayer::parse(
                required("foreground", "app.icon.android.foreground")?,
                "app.icon.android.foreground",
            )?,
            background: if background.starts_with('#') {
                IconBackground::Color(IconColor::try_from(background)?)
            } else {
                IconBackground::Layer(IconLayer::parse(background, "app.icon.android.background")?)
            },
            monochrome: table
                .get("monochrome")
                .map(|item| {
                    let raw = expect_string(item, "app.icon.android.monochrome")?;
                    IconLayer::parse(raw, "app.icon.android.monochrome")
                })
                .transpose()?,
        })
    }

    fn rooted(&self, root: &Path) -> Self {
        Self {
            foreground: self.foreground.rooted(root),
            background: match &self.background {
                IconBackground::Color(color) => IconBackground::Color(color.clone()),
                IconBackground::Layer(layer) => IconBackground::Layer(layer.rooted(root)),
            },
            monochrome: self.monochrome.as_ref().map(|layer| layer.rooted(root)),
        }
    }

    /// Write `mipmap-anydpi-v26/istmo_launcher.xml` and its layers under
    /// `res_dir`, removing the outputs of a previous configuration.
    pub fn write_android(&self, res_dir: &Path) -> Result<(), IconError> {
        let foreground = self
            .foreground
            .write_android(res_dir, LayerSlot::Foreground)?;
        let background = match &self.background {
            IconBackground::Color(color) => {
                LayerSlot::Background.remove_outputs(res_dir)?;
                color.write_android(res_dir)?
            }
            IconBackground::Layer(layer) => {
                remove_if_exists(&res_dir.join(ANDROID_BACKGROUND_COLOR_FILE))?;
                layer.write_android(res_dir, LayerSlot::Background)?
            }
        };
        let monochrome = if let Some(layer) = &self.monochrome {
            let reference = layer.write_android(res_dir, LayerSlot::Monochrome)?;
            format!("    <monochrome android:drawable=\"{reference}\" />\n")
        } else {
            LayerSlot::Monochrome.remove_outputs(res_dir)?;
            String::new()
        };
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
             <!-- Generated by istmo-build from `[app.icon.android]`. -->\n\
             <adaptive-icon xmlns:android=\"http://schemas.android.com/apk/res/android\">\n    \
             <background android:drawable=\"{background}\" />\n    \
             <foreground android:drawable=\"{foreground}\" />\n\
             {monochrome}</adaptive-icon>\n"
        );
        write_bytes_if_changed(&adaptive_icon_file(res_dir), xml.as_bytes())
    }

    /// Remove every output of [`Self::write_android`] under `res_dir`.
    pub fn remove_android(res_dir: &Path) -> Result<(), IconError> {
        remove_if_exists(&adaptive_icon_file(res_dir))?;
        remove_if_exists(&res_dir.join(ANDROID_BACKGROUND_COLOR_FILE))?;
        for slot in [
            LayerSlot::Foreground,
            LayerSlot::Background,
            LayerSlot::Monochrome,
        ] {
            slot.remove_outputs(res_dir)?;
        }
        Ok(())
    }
}

impl IconLayer {
    fn parse(raw: &str, key: &'static str) -> Result<Self, ManifestError> {
        let path = PathBuf::from(raw);
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase);
        match extension.as_deref() {
            Some("png") => Ok(Self::Png(path)),
            Some("xml") => Ok(Self::Vector(path)),
            _ => Err(ManifestError::InvalidValue {
                key: key.to_owned(),
                value: raw.to_owned(),
                expected: "a `.png` image or an Android vector drawable `.xml`",
            }),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Png(path) | Self::Vector(path) => path,
        }
    }

    fn rooted(&self, root: &Path) -> Self {
        match self {
            Self::Png(path) => Self::Png(root.join(path)),
            Self::Vector(path) => Self::Vector(root.join(path)),
        }
    }

    /// Write the layer into `slot` and return its resource reference.
    fn write_android(&self, res_dir: &Path, slot: LayerSlot) -> Result<String, IconError> {
        let name = slot.resource_name();
        match self {
            Self::Png(path) => {
                remove_if_exists(&slot.vector_file(res_dir))?;
                AppIcon::open(path)?.write_mipmaps(res_dir, name, |edge| {
                    edge * ADAPTIVE_EDGE_NUMERATOR / ADAPTIVE_EDGE_DENOMINATOR
                })?;
                Ok(format!("@mipmap/{name}"))
            }
            Self::Vector(path) => {
                slot.remove_mipmaps(res_dir)?;
                let bytes = fs::read(path).map_err(|source| IconError::ReadFile {
                    path: path.clone(),
                    source,
                })?;
                write_bytes_if_changed(&slot.vector_file(res_dir), &bytes)?;
                Ok(format!("@drawable/{name}"))
            }
        }
    }
}

impl TryFrom<&str> for IconColor {
    type Error = ManifestError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        let digits = raw.strip_prefix('#').unwrap_or_default();
        if matches!(digits.len(), 6 | 8) && digits.chars().all(|c| c.is_ascii_hexdigit()) {
            Ok(Self(raw.to_owned()))
        } else {
            Err(ManifestError::InvalidValue {
                key: "app.icon.android.background".to_owned(),
                value: raw.to_owned(),
                expected: "`#RRGGBB` or `#AARRGGBB`, or a `.png` / `.xml` layer",
            })
        }
    }
}

impl IconColor {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Write the colour resource and return its reference.
    fn write_android(&self, res_dir: &Path) -> Result<String, IconError> {
        let name = LayerSlot::Background.resource_name();
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
             <!-- Generated by istmo-build from `[app.icon.android]`. -->\n\
             <resources>\n    <color name=\"{name}\">{}</color>\n</resources>\n",
            self.0
        );
        write_bytes_if_changed(&res_dir.join(ANDROID_BACKGROUND_COLOR_FILE), xml.as_bytes())?;
        Ok(format!("@color/{name}"))
    }
}

impl LayerSlot {
    const fn resource_name(self) -> &'static str {
        match self {
            Self::Foreground => "istmo_launcher_foreground",
            Self::Background => "istmo_launcher_background",
            Self::Monochrome => "istmo_launcher_monochrome",
        }
    }

    fn vector_file(self, res_dir: &Path) -> PathBuf {
        res_dir
            .join("drawable")
            .join(format!("{}.xml", self.resource_name()))
    }

    fn remove_mipmaps(self, res_dir: &Path) -> Result<(), IconError> {
        ANDROID_DENSITIES.iter().try_for_each(|(density, _)| {
            remove_if_exists(
                &res_dir
                    .join(format!("mipmap-{density}"))
                    .join(format!("{}.png", self.resource_name())),
            )
        })
    }

    fn remove_outputs(self, res_dir: &Path) -> Result<(), IconError> {
        remove_if_exists(&self.vector_file(res_dir))?;
        self.remove_mipmaps(res_dir)
    }
}

fn adaptive_icon_file(res_dir: &Path) -> PathBuf {
    res_dir
        .join("mipmap-anydpi-v26")
        .join(format!("{ANDROID_ICON_RESOURCE}.xml"))
}

fn remove_if_exists(path: &Path) -> Result<(), IconError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(IconError::Write {
            path: path.to_path_buf(),
            source,
        }),
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

    fn parse(source: &str) -> Result<IconConfig, ManifestError> {
        let doc: toml_edit::DocumentMut = source.parse().unwrap();
        IconConfig::parse(&doc["icon"])
    }

    fn adaptive(foreground: IconLayer, background: IconBackground) -> AdaptiveIconConfig {
        AdaptiveIconConfig {
            foreground,
            background,
            monochrome: None,
        }
    }

    fn png_layer(dir: &Path, name: &str) -> IconLayer {
        let path = dir.join(name);
        icon().image.save(&path).unwrap();
        IconLayer::Png(path)
    }

    fn vector_layer(dir: &Path, name: &str) -> IconLayer {
        let path = dir.join(name);
        fs::write(&path, "<vector />").unwrap();
        IconLayer::Vector(path)
    }

    #[test]
    fn parses_image_and_adaptive_layers() {
        let icon = parse(
            r##"
[icon]
image = "assets/icon.png"

[icon.android]
foreground = "assets/fg.xml"
background = "#EFE6D4"
monochrome = "assets/mono.PNG"
"##,
        )
        .unwrap();
        assert_eq!(icon.image, Some(PathBuf::from("assets/icon.png")));
        let android = icon.android.unwrap();
        assert_eq!(
            android.foreground,
            IconLayer::Vector(PathBuf::from("assets/fg.xml"))
        );
        assert_eq!(
            android.background,
            IconBackground::Color(IconColor("#EFE6D4".to_owned()))
        );
        assert_eq!(
            android.monochrome,
            Some(IconLayer::Png(PathBuf::from("assets/mono.PNG")))
        );
    }

    #[test]
    fn desktop_source_falls_back_to_image() {
        let mut icon = parse("[icon]\nimage = \"a.png\"\n").unwrap();
        assert_eq!(icon.desktop_source(), Some(Path::new("a.png")));
        icon.desktop = Some(PathBuf::from("b.png"));
        assert_eq!(icon.desktop_source(), Some(Path::new("b.png")));
    }

    #[test]
    fn writes_multi_resolution_ico() {
        let dir = temp_dir("ico");
        let dest = dir.join("app.ico");
        icon().write_ico(&dest).unwrap();
        let decoded =
            image::load_from_memory_with_format(&fs::read(&dest).unwrap(), ImageFormat::Ico)
                .unwrap();
        // The decoder picks the largest frame.
        assert_eq!(decoded.width(), 256);
        let bytes = fs::read(&dest).unwrap();
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 6);
    }

    #[test]
    fn every_icon_key_is_optional() {
        assert_eq!(parse("[icon]\n").unwrap(), IconConfig::default());
    }

    #[test]
    fn rejects_icon_as_string() {
        let err = parse("icon = \"assets/icon.png\"\n").unwrap_err();
        assert!(matches!(err, ManifestError::TypeMismatch { ref key, .. } if key == "app.icon"));
    }

    #[test]
    fn rejects_bad_icon_values() {
        let cases = [
            ("[icon]\nimgae = \"a.png\"\n", "app.icon.imgae"),
            (
                "[icon.android]\nbackground = \"#fff\"\n",
                "app.icon.android.foreground",
            ),
            (
                "[icon.android]\nforeground = \"a.svg\"\nbackground = \"#ffffff\"\n",
                "app.icon.android.foreground",
            ),
            (
                "[icon.android]\nforeground = \"a.xml\"\nbackground = \"#fff\"\n",
                "app.icon.android.background",
            ),
            (
                "[icon.android]\nforeground = \"a.xml\"\nbackground = \"#ggggggg\"\n",
                "app.icon.android.background",
            ),
            (
                "[icon.android]\nforeground = \"a.xml\"\n",
                "app.icon.android.background",
            ),
        ];
        for (source, expected) in cases {
            let key = match parse(source).unwrap_err() {
                ManifestError::UnknownKey { key } | ManifestError::InvalidValue { key, .. } => key,
                ManifestError::Missing { key } => key.to_owned(),
                other => panic!("{source}: {other:?}"),
            };
            assert_eq!(key, expected, "{source}");
        }
    }

    #[test]
    fn rooted_anchors_every_layer() {
        let icon = IconConfig {
            image: Some(PathBuf::from("icon.png")),
            desktop: Some(PathBuf::from("tile.png")),
            android: Some(AdaptiveIconConfig {
                foreground: IconLayer::Vector(PathBuf::from("fg.xml")),
                background: IconBackground::Layer(IconLayer::Png(PathBuf::from("bg.png"))),
                monochrome: Some(IconLayer::Vector(PathBuf::from("mono.xml"))),
            }),
        }
        .rooted(Path::new("/app"));
        let sources: Vec<_> = icon.source_files().collect();
        assert_eq!(
            sources,
            [
                Path::new("/app/icon.png"),
                Path::new("/app/tile.png"),
                Path::new("/app/fg.xml"),
                Path::new("/app/bg.png"),
                Path::new("/app/mono.xml"),
            ]
        );
    }

    #[test]
    fn writes_vector_adaptive_icon_on_a_colour() {
        let dir = temp_dir("adaptive-vector");
        let res = dir.join("res");
        let mut config = adaptive(
            vector_layer(&dir, "fg.xml"),
            IconBackground::Color(IconColor("#EFE6D4".to_owned())),
        );
        config.monochrome = Some(vector_layer(&dir, "mono.xml"));
        config.write_android(&res).unwrap();

        let xml = fs::read_to_string(res.join("mipmap-anydpi-v26/istmo_launcher.xml")).unwrap();
        assert!(
            xml.contains("<background android:drawable=\"@color/istmo_launcher_background\" />")
        );
        assert!(
            xml.contains("<foreground android:drawable=\"@drawable/istmo_launcher_foreground\" />")
        );
        assert!(
            xml.contains("<monochrome android:drawable=\"@drawable/istmo_launcher_monochrome\" />")
        );
        assert_eq!(
            fs::read_to_string(res.join("drawable/istmo_launcher_foreground.xml")).unwrap(),
            "<vector />"
        );
        let colors = fs::read_to_string(res.join(ANDROID_BACKGROUND_COLOR_FILE)).unwrap();
        assert!(colors.contains("<color name=\"istmo_launcher_background\">#EFE6D4</color>"));
    }

    #[test]
    fn writes_png_layers_at_108_dp() {
        let dir = temp_dir("adaptive-png");
        let res = dir.join("res");
        let config = adaptive(
            png_layer(&dir, "fg.png"),
            IconBackground::Layer(png_layer(&dir, "bg.png")),
        );
        config.write_android(&res).unwrap();
        for (density, edge) in ANDROID_DENSITIES {
            let decoded =
                image::open(res.join(format!("mipmap-{density}/istmo_launcher_foreground.png")))
                    .unwrap();
            assert_eq!(decoded.width(), edge * 9 / 4);
        }
        let xml = fs::read_to_string(res.join("mipmap-anydpi-v26/istmo_launcher.xml")).unwrap();
        assert!(xml.contains("@mipmap/istmo_launcher_background"));
        assert!(!xml.contains("monochrome"));
    }

    #[test]
    fn switching_layers_removes_stale_outputs() {
        let dir = temp_dir("adaptive-stale");
        let res = dir.join("res");
        let mut config = adaptive(
            png_layer(&dir, "fg.png"),
            IconBackground::Color(IconColor("#000000".to_owned())),
        );
        config.monochrome = Some(vector_layer(&dir, "mono.xml"));
        config.write_android(&res).unwrap();

        let config = adaptive(
            vector_layer(&dir, "fg.xml"),
            IconBackground::Layer(vector_layer(&dir, "bg.xml")),
        );
        config.write_android(&res).unwrap();
        assert!(
            !res.join("mipmap-xxxhdpi/istmo_launcher_foreground.png")
                .exists()
        );
        assert!(!res.join(ANDROID_BACKGROUND_COLOR_FILE).exists());
        assert!(!res.join("drawable/istmo_launcher_monochrome.xml").exists());
        assert!(res.join("drawable/istmo_launcher_background.xml").is_file());

        AdaptiveIconConfig::remove_android(&res).unwrap();
        assert!(!res.join("mipmap-anydpi-v26/istmo_launcher.xml").exists());
        assert!(!res.join("drawable/istmo_launcher_foreground.xml").exists());
        assert!(!res.join("drawable/istmo_launcher_background.xml").exists());
    }

    #[test]
    fn missing_vector_layer_names_the_file() {
        let dir = temp_dir("adaptive-missing");
        let config = adaptive(
            IconLayer::Vector(dir.join("nope.xml")),
            IconBackground::Color(IconColor("#000000".to_owned())),
        );
        let err = config.write_android(&dir.join("res")).unwrap_err();
        assert!(matches!(err, IconError::ReadFile { ref path, .. } if path.ends_with("nope.xml")));
    }
}
