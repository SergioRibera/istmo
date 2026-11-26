//! The SPA pods the portal's video streams negotiate with.
//!
//! Only shared-memory buffers (`MemFd` / `MemPtr`) are offered: handing
//! a DMA-BUF to a consumer means keeping the `pw_buffer` dequeued past
//! the `process` callback and duplicating its fd, which the `pipewire`
//! crate exposes only through raw pointers. So DMA-BUF tiers are never
//! offered (and the capability report says so), instead of being taken
//! and then copied behind the caller's back.

use pipewire as pw;
use pw::spa::buffer::DataType;
use pw::spa::param::ParamType;
use pw::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pw::spa::param::video::VideoFormat;
use pw::spa::pod::{ChoiceValue, Object, Property, PropertyFlags, Value};
use pw::spa::sys::{
    SPA_META_Header, SPA_META_VideoCrop, SPA_META_VideoDamage, SPA_PARAM_BUFFERS_buffers,
    SPA_PARAM_BUFFERS_dataType, SPA_PARAM_META_size, SPA_PARAM_META_type,
};
use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle, SpaTypes};

use crate::types::{
    BackendId, BufferMemory, CaptureError, CopyStage, FormatRequest, PixelFormat, Size,
};

use super::super::pipewire as pwx;

const ID: Option<BackendId> = Some(BackendId::LinuxPortal);

/// The packed RGB formats the portal can deliver in shared memory, in
/// the order offered when the caller has no preference (compositors
/// render `BGRx` natively).
pub(super) const SHM_FORMATS: [PixelFormat; 4] = [
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
    SHM_FORMATS
        .into_iter()
        .find(|f| spa_format(*f) == Some(format))
}

/// One CPU tier of the request the portal can serve: its index and its
/// formats in the caller's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ShmTier {
    pub(super) tier: u8,
    pub(super) formats: Vec<PixelFormat>,
}

/// The tiers PipeWire shared memory can serve, best first. An empty
/// request is one tier of every format. A tier qualifies when it allows
/// `ShmTransport` (the producer copies into shared memory) and lists CPU
/// choices of the formats above.
pub(super) fn shm_tiers(request: &FormatRequest) -> Vec<ShmTier> {
    if request.tiers.is_empty() {
        return vec![ShmTier {
            tier: 0,
            formats: SHM_FORMATS.to_vec(),
        }];
    }
    request
        .tiers
        .iter()
        .enumerate()
        .filter(|(_, tier)| tier.allows_copy.contains(&CopyStage::ShmTransport))
        .filter_map(|(index, tier)| {
            let mut formats = Vec::new();
            for choice in &tier.choices {
                if choice.memory == BufferMemory::Cpu
                    && spa_format(choice.format).is_some()
                    && !formats.contains(&choice.format)
                {
                    formats.push(choice.format);
                }
            }
            (!formats.is_empty()).then(|| ShmTier {
                tier: u8::try_from(index).unwrap_or(u8::MAX),
                formats,
            })
        })
        .collect()
}

/// The tier a negotiated format belongs to: the first that lists it.
pub(super) fn tier_of(tiers: &[ShmTier], format: PixelFormat) -> Option<u8> {
    tiers
        .iter()
        .find(|t| t.formats.contains(&format))
        .map(|t| t.tier)
}

const fn id(key: u32, value: u32) -> Property {
    Property {
        key,
        flags: PropertyFlags::empty(),
        value: Value::Id(Id(value)),
    }
}

/// One `EnumFormat` pod per tier, in tier order: the producer takes the
/// first it can satisfy.
pub(super) fn enum_formats(
    tiers: &[ShmTier],
    size_hint: Option<Size>,
    fps: u32,
) -> Result<Vec<Vec<u8>>, CaptureError> {
    let size = size_hint
        .filter(|s| !s.is_empty())
        .unwrap_or(Size::new(1920, 1080));
    tiers
        .iter()
        .map(|tier| {
            let formats: Vec<Id> = tier
                .formats
                .iter()
                .filter_map(|f| spa_format(*f))
                .map(|f| Id(f.as_raw()))
                .collect();
            let default = formats
                .first()
                .copied()
                .unwrap_or(Id(VideoFormat::BGRx.as_raw()));
            let object = Object {
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
                        key: FormatProperties::VideoFormat.as_raw(),
                        flags: PropertyFlags::empty(),
                        value: Value::Choice(ChoiceValue::Id(Choice(
                            ChoiceFlags::empty(),
                            ChoiceEnum::Enum {
                                default,
                                alternatives: formats,
                            },
                        ))),
                    },
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
            };
            pwx::serialize(ID, "EnumFormat pod", object)
        })
        .collect()
}

/// `ParamBuffers`: shared memory only, `depth` buffers preferred. Size
/// and stride stay the producer's choice (over-constraining them leaves
/// some producers without an answer).
pub(super) fn buffers(depth: u8) -> Result<Vec<u8>, CaptureError> {
    let types = (1_i32 << DataType::MemFd.as_raw()) | (1_i32 << DataType::MemPtr.as_raw());
    pwx::serialize(
        ID,
        "Buffers pod",
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
                            max: 64,
                        },
                    ))),
                },
                Property {
                    key: SPA_PARAM_BUFFERS_dataType,
                    flags: PropertyFlags::empty(),
                    value: Value::Int(types),
                },
            ],
        },
    )
}

fn meta(kind: u32, size: usize) -> Result<Vec<u8>, CaptureError> {
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
                    value: Value::Int(i32::try_from(size).unwrap_or(i32::MAX)),
                },
            ],
        },
    )
}

/// Damage regions asked for per buffer.
const DAMAGE_REGIONS: usize = 16;

/// The metas requested on every buffer: header (pts), crop, damage. The
/// cursor and video-transform metas are not requested: they are not
/// readable through the `pipewire` crate's safe API.
pub(super) fn metas() -> Result<Vec<Vec<u8>>, CaptureError> {
    let region = size_of::<pw::spa::sys::spa_meta_region>();
    Ok(vec![
        meta(SPA_META_Header, size_of::<pw::spa::sys::spa_meta_header>())?,
        meta(SPA_META_VideoCrop, region)?,
        meta(SPA_META_VideoDamage, region * DAMAGE_REGIONS)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FormatChoice, FormatTier};

    fn valid(bytes: &[u8]) -> bool {
        pw::spa::pod::Pod::from_bytes(bytes).is_some()
    }

    #[test]
    fn cpu_tiers_keep_the_callers_order_and_skip_what_shm_cannot_serve() {
        let request = FormatRequest {
            tiers: vec![
                // DMA-BUF only: never offered.
                FormatTier::new(vec![FormatChoice::dmabuf(PixelFormat::Bgrx8, vec![0])]),
                // CPU but copy-free: shared memory is a copy.
                FormatTier::new(vec![FormatChoice::cpu(PixelFormat::Rgba8)]),
                FormatTier::new(vec![
                    FormatChoice::cpu(PixelFormat::Rgbx8),
                    FormatChoice::cpu(PixelFormat::Nv12),
                    FormatChoice::cpu(PixelFormat::Bgra8),
                ])
                .allowing(CopyStage::ShmTransport),
            ],
        };
        let tiers = shm_tiers(&request);
        assert_eq!(
            tiers,
            [ShmTier {
                tier: 2,
                formats: vec![PixelFormat::Rgbx8, PixelFormat::Bgra8],
            }]
        );
        assert_eq!(tier_of(&tiers, PixelFormat::Bgra8), Some(2));
        assert_eq!(tier_of(&tiers, PixelFormat::Bgrx8), None);
    }

    #[test]
    fn an_empty_request_offers_every_shm_format() {
        let tiers = shm_tiers(&FormatRequest::default());
        assert_eq!(tiers.len(), 1);
        assert_eq!(tiers[0].formats, SHM_FORMATS);
    }

    #[test]
    fn a_dmabuf_only_request_offers_nothing() {
        let request = FormatRequest {
            tiers: vec![FormatTier::new(vec![FormatChoice::dmabuf(
                PixelFormat::Bgrx8,
                Vec::new(),
            )])],
        };
        assert_eq!(shm_tiers(&request).len(), 0);
    }

    #[test]
    fn one_valid_pod_per_tier_in_tier_order() {
        let request = FormatRequest {
            tiers: vec![
                FormatTier::new(vec![FormatChoice::cpu(PixelFormat::Bgrx8)])
                    .allowing(CopyStage::ShmTransport),
                FormatTier::cpu_rgb(),
            ],
        };
        let tiers = shm_tiers(&request);
        let pods = enum_formats(&tiers, Some(Size::new(2560, 1440)), 60).expect("pods");
        assert_eq!(pods.len(), 2);
        assert!(pods.iter().all(|p| valid(p)));
        assert!(
            pods[0].len() < pods[1].len(),
            "the second tier lists four formats"
        );
        assert!(valid(&buffers(4).expect("buffers")));
        assert!(metas().expect("metas").iter().all(|m| valid(m)));
    }

    #[test]
    fn formats_round_trip_through_spa() {
        for format in SHM_FORMATS {
            let spa = spa_format(format).expect("shm format");
            assert_eq!(pixel_format(spa), Some(format));
        }
        assert_eq!(spa_format(PixelFormat::Nv12), None);
    }
}
