//! PCM out of an audio `CMSampleBuffer`: its stream description and the
//! `AudioBufferList` behind it, read into `f32` channels.

use std::ptr::NonNull;

use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved,
    kAudioFormatLinearPCM,
};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{
    CMAudioFormatDescriptionGetStreamBasicDescription, CMBlockBuffer,
    kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment,
};

use crate::types::CaptureError;

use super::super::super::sck_map::ID;
use super::Sample;

/// One chunk of audio as `f32`: one `Vec` per channel when the source is
/// planar (SCK's own layout), or one interleaved `Vec`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AudioChunk {
    pub(crate) rate: f64,
    pub(crate) channels: u32,
    pub(crate) planar: bool,
    /// Planar: one entry per channel; interleaved: a single entry.
    pub(crate) samples: Vec<Vec<f32>>,
}

/// Buffers an `AudioBufferList` may describe (SCK delivers at most two
/// channels; a margin for other layouts).
const MAX_BUFFERS: usize = 16;

/// `AudioBufferList` with room for [`MAX_BUFFERS`] buffers, laid out
/// like the C struct.
#[repr(C)]
struct BufferList {
    count: u32,
    buffers: [AudioBuffer; MAX_BUFFERS],
}

impl Sample {
    /// The chunk's PCM.
    ///
    /// # Errors
    /// The buffer is not 32-bit float linear PCM (what SCK delivers), or
    /// Core Media could not hand its data out.
    pub(crate) fn audio(&self) -> Result<AudioChunk, CaptureError> {
        let fail = |message: &str| CaptureError::protocol(ID, "audio", message);
        // SAFETY: a getter on a valid sample buffer; the binding retains
        // the result.
        let description =
            unsafe { self.0.format_description() }.ok_or_else(|| fail("no format description"))?;
        // SAFETY: an audio sample buffer's description is an audio format
        // description.
        let asbd = unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(&description) };
        // SAFETY: null, or a description that lives as long as
        // `description`.
        let asbd =
            unsafe { asbd.as_ref() }.ok_or_else(|| fail("not an audio format description"))?;
        if asbd.mFormatID != kAudioFormatLinearPCM
            || asbd.mFormatFlags & kAudioFormatFlagIsFloat == 0
            || asbd.mBitsPerChannel != 32
        {
            return Err(fail("not 32-bit float PCM"));
        }
        let planar = asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0;
        let mut list = BufferList {
            count: 0,
            buffers: [AudioBuffer {
                mNumberChannels: 0,
                mDataByteSize: 0,
                mData: std::ptr::null_mut(),
            }; MAX_BUFFERS],
        };
        let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
        // SAFETY: `list` is writable and `size_of::<BufferList>()` bytes
        // long, laid out as an `AudioBufferList` with `MAX_BUFFERS`
        // entries; the retained block buffer comes back in `block`.
        let status = unsafe {
            self.0.audio_buffer_list_with_retained_block_buffer(
                std::ptr::null_mut(),
                std::ptr::from_mut(&mut list).cast::<AudioBufferList>(),
                size_of::<BufferList>(),
                None,
                None,
                kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment,
                &raw mut block,
            )
        };
        if status != 0 {
            return Err(CaptureError::os(ID, "audio buffer list", i64::from(status)));
        }
        // SAFETY: on success `block` carries a +1 reference that keeps the
        // buffer list's data alive; this value releases it.
        let _block = NonNull::new(block).map(|block| unsafe { CFRetained::from_raw(block) });
        let count = usize::try_from(list.count).unwrap_or(0).min(MAX_BUFFERS);
        let samples = list
            .buffers
            .iter()
            .take(count)
            .map(|buffer| {
                let floats = usize::try_from(buffer.mDataByteSize).unwrap_or(0) / size_of::<f32>();
                NonNull::new(buffer.mData.cast::<f32>()).map_or_else(Vec::new, |data| {
                    // SAFETY: the buffer holds `mDataByteSize` bytes of
                    // aligned `f32` (16-byte alignment asked for), alive
                    // while `_block` is.
                    unsafe { std::slice::from_raw_parts(data.as_ptr(), floats) }.to_vec()
                })
            })
            .collect();
        Ok(AudioChunk {
            rate: asbd.mSampleRate,
            channels: asbd.mChannelsPerFrame,
            planar,
            samples,
        })
    }
}
