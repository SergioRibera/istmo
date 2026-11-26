//! The SPA pods the portal's video streams negotiate with.
//!
//! Tiers become `EnumFormat` pods in tier order: one per DMA-BUF format
//! (its modifiers as a `MANDATORY | DONT_FIXATE` choice, so the producer
//! fixates one it can allocate) and one per CPU tier (shared memory,
//! `MemFd`). DMA-BUF modifiers are the caller's, intersected with what the
//! compositor imports (its `linux-dmabuf` feedback), or the compositor's
//! when the caller listed none. Buffers are offered as DMA-BUF (with an
//! explicit-sync variant first) or `MemFd`; `MemPtr` is never accepted,
//! since its pixels could only leave through a second copy.

use std::collections::HashMap;

use pipewire as pw;
use pw::spa::param::ParamType;
use pw::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pw::spa::param::video::VideoFormat;
use pw::spa::pod::{ChoiceValue, Object, Property, PropertyFlags, Value};
use pw::spa::sys::{
    SPA_PARAM_BUFFERS_buffers, SPA_PARAM_BUFFERS_dataType, SPA_PARAM_META_size, SPA_PARAM_META_type,
};
use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle, SpaTypes};

use crate::types::{
    BackendId, BufferMemory, CaptureError, CopyStage, FormatRequest, PixelFormat, Size,
};

use super::super::pipewire as pwx;
use super::raw;

const ID: Option<BackendId> = Some(BackendId::LinuxPortal);

/// `SPA_PARAM_BUFFERS_metaType` (`spa/param/buffers.h`): the metas a
/// buffer must carry, as a flags choice of `1 << SPA_META_*`.
const SPA_PARAM_BUFFERS_METATYPE: u32 = 7;
/// `SPA_POD_PROP_FLAG_DONT_FIXATE` (PipeWire 0.3.33; the binding is behind a
/// version feature this crate does not raise its floor for).
const PROP_FLAG_DONT_FIXATE: u32 = 1 << 4;

/// The packed RGB formats the portal can deliver, in the order offered
/// when the caller has no preference (compositors render `BGRx`).
pub(super) const RGB_FORMATS: [PixelFormat; 4] = [
    PixelFormat::Bgrx8,
    PixelFormat::Bgra8,
    PixelFormat::Rgbx8,
    PixelFormat::Rgba8,
];

pub(super) const fn spa_format(format: PixelFormat) -> Option<VideoFormat> {
    Some(match format {
        PixelFormat::Bgrx8 => VideoFormat::BGRx,
        PixelFormat::Bgra8 => VideoFormat::BGRA,
        PixelFormat::Rgbx8 => VideoFormat::RGBx,
        PixelFormat::Rgba8 => VideoFormat::RGBA,
        PixelFormat::Xr30
        | PixelFormat::Xb30
        | PixelFormat::Ar30
        | PixelFormat::Rgba16f
        | PixelFormat::Nv12
        | PixelFormat::P010 => return None,
    })
}

pub(super) fn pixel_format(format: VideoFormat) -> Option<PixelFormat> {
    RGB_FORMATS
        .into_iter()
        .find(|f| spa_format(*f) == Some(format))
}

/// One acceptable negotiation of a tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PortalChoice {
    /// DMA-BUF in `format` with one of `modifiers` (caller's order).
    DmaBuf {
        format: PixelFormat,
        modifiers: Vec<u64>,
    },
    /// Shared memory (`MemFd`) in one of `formats`.
    Shm { formats: Vec<PixelFormat> },
}

/// One tier of the request the portal can serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PortalTier {
    pub(super) tier: u8,
    pub(super) choices: Vec<PortalChoice>,
}

/// What the compositor imports, by DRM fourcc (its `linux-dmabuf`
/// feedback main tranche); `None` when unknown (no Wayland display).
pub(super) type Imports = Option<HashMap<u32, Vec<u64>>>;

/// The DMA-BUF modifiers to offer for `format`: the caller's (all of them
/// when the compositor is unknown, else those it imports), or the
/// compositor's when the caller listed none.
fn modifiers(format: PixelFormat, wanted: &[u64], imports: &Imports) -> Vec<u64> {
    let known = imports
        .as_ref()
        .and_then(|i| i.get(&format.drm_fourcc()))
        .cloned();
    match (wanted.is_empty(), known) {
        (true, Some(known)) => known,
        (true, None) => Vec::new(),
        (false, Some(known)) => wanted
            .iter()
            .copied()
            .filter(|m| known.contains(m))
            .collect(),
        (false, None) => wanted.to_vec(),
    }
}

/// The tiers the portal can serve, best first. An empty request is one
/// tier: every RGB format as DMA-BUF (compositor modifiers), then shared
/// memory; with `cpu_only` (`CopyToCpu`) only shared memory, the memory
/// the read-back works for. A CPU choice needs a tier that allows
/// `ShmTransport`.
pub(super) fn tiers(request: &FormatRequest, imports: &Imports, cpu_only: bool) -> Vec<PortalTier> {
    if request.tiers.is_empty() {
        let mut choices: Vec<PortalChoice> = if cpu_only {
            Vec::new()
        } else {
            RGB_FORMATS
                .into_iter()
                .filter_map(|format| {
                    let modifiers = modifiers(format, &[], imports);
                    (!modifiers.is_empty()).then_some(PortalChoice::DmaBuf { format, modifiers })
                })
                .collect()
        };
        choices.push(PortalChoice::Shm {
            formats: RGB_FORMATS.to_vec(),
        });
        return vec![PortalTier { tier: 0, choices }];
    }
    request
        .tiers
        .iter()
        .enumerate()
        .filter_map(|(index, tier)| {
            let mut choices = Vec::new();
            let mut shm = Vec::new();
            for choice in &tier.choices {
                if spa_format(choice.format).is_none() {
                    continue;
                }
                match choice.memory {
                    BufferMemory::DmaBuf if !cpu_only => {
                        let modifiers = modifiers(choice.format, &choice.modifiers, imports);
                        if !modifiers.is_empty() {
                            choices.push(PortalChoice::DmaBuf {
                                format: choice.format,
                                modifiers,
                            });
                        }
                    }
                    BufferMemory::Cpu
                        if tier.allows_copy.contains(&CopyStage::ShmTransport)
                            && !shm.contains(&choice.format) =>
                    {
                        shm.push(choice.format);
                    }
                    _ => {}
                }
            }
            if !shm.is_empty() {
                choices.push(PortalChoice::Shm { formats: shm });
            }
            (!choices.is_empty()).then(|| PortalTier {
                tier: u8::try_from(index).unwrap_or(u8::MAX),
                choices,
            })
        })
        .collect()
}

/// The tier a negotiated format belongs to: the first listing it with
/// that memory (and, for DMA-BUF, that modifier).
pub(super) fn tier_of(
    tiers: &[PortalTier],
    format: PixelFormat,
    modifier: Option<u64>,
) -> Option<u8> {
    tiers
        .iter()
        .find(|t| {
            t.choices.iter().any(|c| match (c, modifier) {
                (
                    PortalChoice::DmaBuf {
                        format: f,
                        modifiers,
                    },
                    Some(m),
                ) => *f == format && modifiers.contains(&m),
                (PortalChoice::Shm { formats }, None) => formats.contains(&format),
                _ => false,
            })
        })
        .map(|t| t.tier)
}

/// Whether any tier offers DMA-BUF.
#[cfg(test)]
pub(super) fn offers_dmabuf(tiers: &[PortalTier]) -> bool {
    tiers.iter().any(|t| {
        t.choices
            .iter()
            .any(|c| matches!(c, PortalChoice::DmaBuf { .. }))
    })
}

/// Whether any tier offers shared memory.
pub(super) fn offers_shm(tiers: &[PortalTier]) -> bool {
    tiers.iter().any(|t| {
        t.choices
            .iter()
            .any(|c| matches!(c, PortalChoice::Shm { .. }))
    })
}

const fn id(key: u32, value: u32) -> Property {
    Property {
        key,
        flags: PropertyFlags::empty(),
        value: Value::Id(Id(value)),
    }
}

fn skeleton(size: Size, fps: u32) -> Object {
    Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: vec![
            id(
                FormatProperties::MediaType.as_raw(),
                MediaType::Video.as_raw(),
            ),
            id(
                FormatProperties::MediaSubtype.as_raw(),
                MediaSubtype::Raw.as_raw(),
            ),
            Property {
                key: FormatProperties::VideoSize.as_raw(),
                flags: PropertyFlags::empty(),
                value: Value::Choice(ChoiceValue::Rectangle(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Rectangle {
                            width: size.width,
                            height: size.height,
                        },
                        min: Rectangle {
                            width: 1,
                            height: 1,
                        },
                        max: Rectangle {
                            width: 16_384,
                            height: 16_384,
                        },
                    },
                ))),
            },
            Property {
                key: FormatProperties::VideoFramerate.as_raw(),
                flags: PropertyFlags::empty(),
                value: Value::Choice(ChoiceValue::Fraction(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Fraction {
                            num: fps.max(1),
                            denom: 1,
                        },
                        min: Fraction { num: 0, denom: 1 },
                        max: Fraction {
                            num: 1000,
                            denom: 1,
                        },
                    },
                ))),
            },
        ],
    }
}

fn format_choice(formats: &[PixelFormat]) -> Property {
    let formats: Vec<Id> = formats
        .iter()
        .filter_map(|f| spa_format(*f))
        .map(|f| Id(f.as_raw()))
        .collect();
    let default = formats
        .first()
        .copied()
        .unwrap_or(Id(VideoFormat::BGRx.as_raw()));
    Property {
        key: FormatProperties::VideoFormat.as_raw(),
        flags: PropertyFlags::empty(),
        value: Value::Choice(ChoiceValue::Id(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Enum {
                default,
                alternatives: formats,
            },
        ))),
    }
}

/// `VideoModifier` as a `MANDATORY | DONT_FIXATE` choice: the producer
/// picks the one it can allocate.
fn modifier_choice(modifiers: &[u64]) -> Property {
    let values: Vec<i64> = modifiers.iter().map(|m| m.cast_signed()).collect();
    Property {
        key: FormatProperties::VideoModifier.as_raw(),
        flags: PropertyFlags::MANDATORY | PropertyFlags::from_bits_retain(PROP_FLAG_DONT_FIXATE),
        value: Value::Choice(ChoiceValue::Long(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Enum {
                default: values.first().copied().unwrap_or(0),
                alternatives: values,
            },
        ))),
    }
}

/// The `EnumFormat` pods, tier by tier, choice by choice: the producer
/// takes the first it can satisfy.
pub(super) fn enum_formats(
    tiers: &[PortalTier],
    size_hint: Option<Size>,
    fps: u32,
) -> Result<Vec<Vec<u8>>, CaptureError> {
    let size = size_hint
        .filter(|s| !s.is_empty())
        .unwrap_or(Size::new(1920, 1080));
    tiers
        .iter()
        .flat_map(|tier| &tier.choices)
        .map(|choice| {
            let mut object = skeleton(size, fps);
            match choice {
                PortalChoice::DmaBuf { format, modifiers } => {
                    object.properties.push(format_choice(&[*format]));
                    object.properties.push(modifier_choice(modifiers));
                }
                PortalChoice::Shm { formats } => object.properties.push(format_choice(formats)),
            }
            pwx::serialize(ID, "EnumFormat pod", object)
        })
        .collect()
}

/// `pw_stream`'s own per-stream buffer cap.
const MAX_BUFFERS: i32 = 64;

fn buffers_object(depth: u8, data_types: i32) -> Object {
    Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![
            Property {
                key: SPA_PARAM_BUFFERS_buffers,
                flags: PropertyFlags::empty(),
                value: Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: i32::from(depth.max(2)),
                        min: 1,
                        max: MAX_BUFFERS,
                    },
                ))),
            },
            Property {
                key: SPA_PARAM_BUFFERS_dataType,
                flags: PropertyFlags::empty(),
                value: Value::Int(data_types),
            },
        ],
    }
}

/// `ParamBuffers` for the negotiated memory, `depth` buffers preferred.
/// Size and stride stay the producer's choice (over-constraining them
/// leaves some producers without an answer). DMA-BUF first asks for the
/// explicit-sync variant (buffers carrying `SyncTimeline`); a producer
/// without explicit sync fails to intersect it and takes the plain one.
pub(super) fn buffers(depth: u8, dmabuf: bool) -> Result<Vec<Vec<u8>>, CaptureError> {
    if !dmabuf {
        return Ok(vec![pwx::serialize(
            ID,
            "Buffers pod",
            buffers_object(depth, 1_i32 << raw::DATA_MEM_FD),
        )?]);
    }
    let dma = 1_i32 << raw::DATA_DMA_BUF;
    let sync = 1_i32 << raw::META_SYNC_TIMELINE;
    let mut explicit = buffers_object(depth, dma);
    explicit.properties.push(Property {
        key: SPA_PARAM_BUFFERS_METATYPE,
        flags: PropertyFlags::MANDATORY,
        value: Value::Choice(ChoiceValue::Int(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Flags {
                default: sync,
                flags: vec![sync],
            },
        ))),
    });
    Ok(vec![
        pwx::serialize(ID, "explicit-sync Buffers pod", explicit)?,
        pwx::serialize(ID, "Buffers pod", buffers_object(depth, dma))?,
    ])
}

fn meta(kind: u32, size: Value) -> Result<Vec<u8>, CaptureError> {
    pwx::serialize(
        ID,
        "Meta pod",
        Object {
            type_: SpaTypes::ObjectParamMeta.as_raw(),
            id: ParamType::Meta.as_raw(),
            properties: vec![
                id(SPA_PARAM_META_type, kind),
                Property {
                    key: SPA_PARAM_META_size,
                    flags: PropertyFlags::empty(),
                    value: size,
                },
            ],
        },
    )
}

fn fixed(size: usize) -> Value {
    Value::Int(i32::try_from(size).unwrap_or(i32::MAX))
}

/// Damage regions asked for per buffer.
const DAMAGE_REGIONS: usize = 16;
/// A cursor meta with room for a 64x64 ARGB bitmap after the position
/// (the bitmap itself is not read; producers size the meta from it).
const CURSOR_META: usize = size_of::<pw::spa::sys::spa_meta_cursor>()
    + size_of::<pw::spa::sys::spa_meta_bitmap>()
    + 64 * 64 * 4;

/// The metas requested on every buffer: header (pts), crop, damage, the
/// video transform (rotated outputs), the cursor when it travels as
/// metadata, and the sync timeline on DMA-BUF.
pub(super) fn metas(cursor: bool, dmabuf: bool) -> Result<Vec<Vec<u8>>, CaptureError> {
    let region = size_of::<pw::spa::sys::spa_meta_region>();
    let mut pods = vec![
        meta(
            raw::META_HEADER,
            fixed(size_of::<pw::spa::sys::spa_meta_header>()),
        )?,
        meta(raw::META_VIDEO_CROP, fixed(region))?,
        meta(raw::META_VIDEO_DAMAGE, fixed(region * DAMAGE_REGIONS))?,
        meta(raw::META_VIDEO_TRANSFORM, fixed(size_of::<u32>()))?,
    ];
    if cursor {
        let min = i32::try_from(size_of::<pw::spa::sys::spa_meta_cursor>()).unwrap_or(i32::MAX);
        let full = i32::try_from(CURSOR_META).unwrap_or(i32::MAX);
        pods.push(meta(
            raw::META_CURSOR,
            Value::Choice(ChoiceValue::Int(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Range {
                    default: full,
                    min,
                    max: full,
                },
            ))),
        )?);
    }
    if dmabuf {
        pods.push(meta(
            raw::META_SYNC_TIMELINE,
            fixed(size_of::<raw::MetaSyncTimeline>()),
        )?);
    }
    Ok(pods)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FormatChoice, FormatTier};

    fn valid(bytes: &[u8]) -> bool {
        pw::spa::pod::Pod::from_bytes(bytes).is_some()
    }

    const NV_TILED: u64 = 0x0300_0000_0060_6014;

    #[expect(
        clippy::unnecessary_wraps,
        reason = "the shape every tier function takes"
    )]
    fn imports() -> Imports {
        Some(HashMap::from([(
            PixelFormat::Bgrx8.drm_fourcc(),
            vec![NV_TILED, 0],
        )]))
    }

    #[test]
    fn tiers_keep_the_callers_order_and_skip_what_cannot_be_served() {
        let request = FormatRequest {
            tiers: vec![
                // A modifier the compositor does not import: dropped.
                FormatTier::new(vec![FormatChoice::dmabuf(PixelFormat::Bgrx8, vec![7])]),
                // CPU but copy-free: shared memory is a copy.
                FormatTier::new(vec![FormatChoice::cpu(PixelFormat::Rgba8)]),
                FormatTier::new(vec![
                    FormatChoice::dmabuf(PixelFormat::Bgrx8, Vec::new()),
                    FormatChoice::cpu(PixelFormat::Rgbx8),
                    FormatChoice::cpu(PixelFormat::Nv12),
                    FormatChoice::cpu(PixelFormat::Bgra8),
                ])
                .allowing(CopyStage::ShmTransport),
            ],
        };
        let tiers = tiers(&request, &imports(), false);
        assert_eq!(
            tiers,
            [PortalTier {
                tier: 2,
                choices: vec![
                    PortalChoice::DmaBuf {
                        format: PixelFormat::Bgrx8,
                        modifiers: vec![NV_TILED, 0],
                    },
                    PortalChoice::Shm {
                        formats: vec![PixelFormat::Rgbx8, PixelFormat::Bgra8],
                    },
                ],
            }]
        );
        assert_eq!(tier_of(&tiers, PixelFormat::Bgra8, None), Some(2));
        assert_eq!(tier_of(&tiers, PixelFormat::Bgrx8, Some(NV_TILED)), Some(2));
        assert_eq!(tier_of(&tiers, PixelFormat::Bgrx8, Some(9)), None);
        assert_eq!(tier_of(&tiers, PixelFormat::Bgrx8, None), None);
    }

    #[test]
    fn caller_modifiers_are_kept_in_order_and_intersected() {
        let request = FormatRequest {
            tiers: vec![FormatTier::new(vec![FormatChoice::dmabuf(
                PixelFormat::Bgrx8,
                vec![0, 5, NV_TILED],
            )])],
        };
        let known = tiers(&request, &imports(), false);
        assert_eq!(
            known[0].choices,
            [PortalChoice::DmaBuf {
                format: PixelFormat::Bgrx8,
                modifiers: vec![0, NV_TILED],
            }]
        );
        // Without a compositor to ask, the caller's list stands.
        let unknown = tiers(&request, &None, false);
        assert_eq!(
            unknown[0].choices,
            [PortalChoice::DmaBuf {
                format: PixelFormat::Bgrx8,
                modifiers: vec![0, 5, NV_TILED],
            }]
        );
    }

    #[test]
    fn an_empty_request_offers_dmabuf_first_then_shared_memory() {
        let tiers = tiers(&FormatRequest::default(), &imports(), false);
        assert_eq!(tiers.len(), 1);
        assert!(matches!(
            tiers[0].choices.first(),
            Some(PortalChoice::DmaBuf {
                format: PixelFormat::Bgrx8,
                ..
            })
        ));
        assert_eq!(
            tiers[0].choices.last(),
            Some(&PortalChoice::Shm {
                formats: RGB_FORMATS.to_vec()
            })
        );
        let cpu = super::tiers(&FormatRequest::default(), &imports(), true);
        assert!(!offers_dmabuf(&cpu), "CopyToCpu reads shared memory");
        assert!(offers_shm(&cpu));
    }

    #[test]
    fn one_valid_pod_per_choice_in_tier_order() {
        let request = FormatRequest {
            tiers: vec![
                FormatTier::new(vec![FormatChoice::dmabuf(PixelFormat::Bgrx8, Vec::new())]),
                FormatTier::cpu_rgb(),
            ],
        };
        let tiers = tiers(&request, &imports(), false);
        let pods = enum_formats(&tiers, Some(Size::new(2560, 1440)), 60).expect("pods");
        assert_eq!(pods.len(), 2);
        assert!(pods.iter().all(|p| valid(p)));
        for dmabuf in [false, true] {
            assert!(
                buffers(4, dmabuf)
                    .expect("buffers")
                    .iter()
                    .all(|p| valid(p))
            );
            for cursor in [false, true] {
                assert!(
                    metas(cursor, dmabuf)
                        .expect("metas")
                        .iter()
                        .all(|m| valid(m))
                );
            }
        }
    }

    #[test]
    fn explicit_sync_buffers_param_differs_from_the_plain_one() {
        let pods = buffers(4, true).expect("buffers");
        assert_eq!(pods.len(), 2);
        assert_ne!(pods[0], pods[1]);
        assert_eq!(buffers(4, false).expect("shm").len(), 1);
    }

    #[test]
    fn formats_round_trip_through_spa() {
        for format in RGB_FORMATS {
            let spa = spa_format(format).expect("rgb format");
            assert_eq!(pixel_format(spa), Some(format));
        }
        assert_eq!(spa_format(PixelFormat::Nv12), None);
    }
}
