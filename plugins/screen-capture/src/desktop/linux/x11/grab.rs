//! Reading pixels: MIT-SHM into a memfd segment (no socket copy), else
//! `GetImage` over the socket; both are the server copying into CPU
//! memory (`CopyStage::ShmTransport`). Window content comes from a
//! Composite pixmap, so occluded parts read correctly.

use std::os::fd::OwnedFd;

use rustix::fs::{MemfdFlags, ftruncate, memfd_create};
use rustix::io::{fcntl_dupfd_cloexec, pread};
use x11rb::connection::Connection as _;
use x11rb::protocol::composite::{self, ConnectionExt as _};
use x11rb::protocol::shm::{self, ConnectionExt as _};
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{self, ConnectionExt as _, ImageFormat, ImageOrder};

use crate::types::{CaptureError, PixelFormat, Point, Rect, Size};

use super::conn::{X11, lost_or, x_err};

/// The packed format X delivers for a pixmap depth and layout, if the
/// plugin models it. X11 images are in the server's byte order; on a
/// little-endian (`LSB_FIRST`) server a 32-bit `0x00RRGGBB` pixel is the
/// bytes `B, G, R, X`.
#[must_use]
pub(super) const fn pixel_format(
    depth: u8,
    bits_per_pixel: u8,
    lsb_first: bool,
) -> Option<PixelFormat> {
    if !lsb_first || bits_per_pixel != 32 {
        return None;
    }
    match depth {
        24 => Some(PixelFormat::Bgrx8),
        32 => Some(PixelFormat::Bgra8),
        30 => Some(PixelFormat::Xr30),
        _ => None,
    }
}

/// Row pitch of a Z-pixmap image.
#[must_use]
pub(super) const fn stride(width: u32, bits_per_pixel: u8, scanline_pad: u8) -> u32 {
    let pad = if scanline_pad == 0 {
        32
    } else {
        scanline_pad as u32
    };
    (width * bits_per_pixel as u32).div_ceil(pad) * pad / 8
}

/// One read image.
#[derive(Debug)]
pub(super) struct Grab {
    pub(super) size: Size,
    pub(super) stride: u32,
    pub(super) format: PixelFormat,
    pub(super) bytes: Vec<u8>,
}

/// A memfd attached to the server as an MIT-SHM segment.
#[derive(Debug)]
struct Segment {
    id: shm::Seg,
    fd: OwnedFd,
    capacity: usize,
}

/// Reads drawables of one connection.
#[derive(Debug, Default)]
pub(super) struct Grabber {
    segment: Option<Segment>,
    /// Attaching failed once (remote display, no fd passing): socket only.
    shm_broken: bool,
    /// Windows this grabber redirected (once each), undone on release.
    redirected: Vec<xproto::Window>,
}

fn layout_of(x: &X11, depth: u8) -> Result<(u8, u8, bool), CaptureError> {
    let setup = x.conn.setup();
    let format = setup
        .pixmap_formats
        .iter()
        .find(|f| f.depth == depth)
        .ok_or_else(|| {
            x_err(
                "pixmap format",
                format!("no pixmap format for depth {depth}"),
            )
        })?;
    Ok((
        format.bits_per_pixel,
        format.scanline_pad,
        setup.image_byte_order == ImageOrder::LSB_FIRST,
    ))
}

impl Grabber {
    fn segment(&mut self, x: &X11, len: usize) -> Option<&Segment> {
        if self.shm_broken || !x.ext.shm_fd {
            return None;
        }
        if self.segment.as_ref().is_some_and(|s| s.capacity >= len) {
            return self.segment.as_ref();
        }
        if let Some(old) = self.segment.take() {
            let _detached = x.conn.shm_detach(old.id);
        }
        match Self::attach(x, len) {
            Ok(segment) => {
                self.segment = Some(segment);
                self.segment.as_ref()
            }
            Err(err) => {
                tracing::debug!(%err, "x11: MIT-SHM attach failed, reading over the socket");
                self.shm_broken = true;
                None
            }
        }
    }

    fn attach(x: &X11, len: usize) -> Result<Segment, CaptureError> {
        let fd =
            memfd_create("istmo-x11-shm", MemfdFlags::CLOEXEC).map_err(|e| x_err("memfd", e))?;
        ftruncate(&fd, len as u64).map_err(|e| x_err("memfd size", e))?;
        // The server gets its own descriptor; ours stays for reading.
        let server = fcntl_dupfd_cloexec(&fd, 0).map_err(|e| x_err("dup segment", e))?;
        let id = x.conn.generate_id().map_err(|e| x_err("shm id", e))?;
        x.conn
            .shm_attach_fd(id, server, false)
            .map_err(|e| x_err("shm attach", e))?
            .check()
            .map_err(|e| x_err("shm attach", e))?;
        Ok(Segment {
            id,
            fd,
            capacity: len,
        })
    }

    /// Read `rect` of `drawable` (depth `depth`) into `reuse` (resized as
    /// needed).
    ///
    /// # Errors
    /// `SourceLost` when the drawable is gone, otherwise the failing step.
    pub(super) fn grab(
        &mut self,
        x: &X11,
        drawable: xproto::Drawable,
        depth: u8,
        rect: Rect,
        mut reuse: Vec<u8>,
    ) -> Result<Grab, CaptureError> {
        let (bpp, pad, lsb) = layout_of(x, depth)?;
        let format = pixel_format(depth, bpp, lsb).ok_or_else(|| {
            x_err(
                "pixel format",
                format!("depth {depth} at {bpp} bpp is not supported"),
            )
        })?;
        let size = rect.size();
        let stride = stride(size.width, bpp, pad);
        let len = stride as usize * size.height as usize;
        let (x0, y0, w, h) = (
            i16::try_from(rect.x).map_err(|e| x_err("grab", e))?,
            i16::try_from(rect.y).map_err(|e| x_err("grab", e))?,
            u16::try_from(size.width).map_err(|e| x_err("grab", e))?,
            u16::try_from(size.height).map_err(|e| x_err("grab", e))?,
        );
        if let Some(segment) = self.segment(x, len) {
            let (id, fd) = (segment.id, &segment.fd);
            let reply = x
                .conn
                .shm_get_image(
                    drawable,
                    x0,
                    y0,
                    w,
                    h,
                    u32::MAX,
                    ImageFormat::Z_PIXMAP.into(),
                    id,
                    0,
                )
                .map_err(|e| x_err("shm get image", e))?
                .reply()
                .map_err(|e| lost_or("shm get image", e))?;
            if reply.depth != depth {
                return Err(x_err(
                    "shm get image",
                    format!("depth {} instead of {depth}", reply.depth),
                ));
            }
            reuse.resize(len, 0);
            let mut read = 0;
            while read < len {
                let n = pread(fd, &mut reuse[read..], read as u64)
                    .map_err(|e| x_err("read segment", e))?;
                if n == 0 {
                    return Err(x_err("read segment", "short segment"));
                }
                read += n;
            }
            return Ok(Grab {
                size,
                stride,
                format,
                bytes: reuse,
            });
        }
        let reply = x
            .conn
            .get_image(ImageFormat::Z_PIXMAP, drawable, x0, y0, w, h, u32::MAX)
            .map_err(|e| x_err("get image", e))?
            .reply()
            .map_err(|e| lost_or("get image", e))?;
        if reply.data.len() < len {
            return Err(x_err(
                "get image",
                format!("{} bytes, {len} expected", reply.data.len()),
            ));
        }
        Ok(Grab {
            size,
            stride,
            format,
            bytes: reply.data,
        })
    }

    /// Read a window's content through a Composite pixmap (occluded parts
    /// included), or the window itself without Composite.
    ///
    /// # Errors
    /// `SourceLost` when the window is gone or unmapped.
    pub(super) fn grab_window(
        &mut self,
        x: &X11,
        window: xproto::Window,
        reuse: Vec<u8>,
    ) -> Result<Grab, CaptureError> {
        let geometry = x
            .conn
            .get_geometry(window)
            .map_err(|e| x_err("get geometry", e))?
            .reply()
            .map_err(|e| lost_or("get geometry", e))?;
        let border = i32::from(geometry.border_width);
        let rect = Rect::new(
            border,
            border,
            u32::from(geometry.width),
            u32::from(geometry.height),
        );
        if !x.ext.composite {
            return self.grab(
                x,
                window,
                geometry.depth,
                Rect::from_size(rect.size()),
                reuse,
            );
        }
        // Automatic redirection keeps the window on screen. Once per
        // window, undone in `release`; a compositing manager may already
        // redirect it (then this is refused and nothing is to undo).
        if !self.redirected.contains(&window) {
            let redirected = x
                .conn
                .composite_redirect_window(window, composite::Redirect::AUTOMATIC)
                .map_err(|e| x_err("redirect window", e))?
                .check()
                .is_ok();
            if redirected {
                self.redirected.push(window);
            }
        }
        let pixmap = x.conn.generate_id().map_err(|e| x_err("pixmap id", e))?;
        x.conn
            .composite_name_window_pixmap(window, pixmap)
            .map_err(|e| x_err("name window pixmap", e))?
            .check()
            .map_err(|e| lost_or("name window pixmap", e))?;
        let grabbed = self.grab(x, pixmap, geometry.depth, rect, reuse);
        let _freed = x.conn.free_pixmap(pixmap);
        grabbed
    }

    /// Detach the segment (also implicit when the connection closes).
    pub(super) fn release(&mut self, x: &X11) {
        if let Some(segment) = self.segment.take() {
            let _detached = x.conn.shm_detach(segment.id);
        }
        for window in self.redirected.drain(..) {
            // Fails only for a window that is already gone.
            let _undone = x
                .conn
                .composite_unredirect_window(window, composite::Redirect::AUTOMATIC);
        }
    }
}

/// The cursor as XFixes reports it: premultiplied ARGB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Cursor {
    /// Pointer position on the root window.
    pub(super) position: Point,
    pub(super) hotspot: Point,
    pub(super) size: Size,
    pub(super) pixels: Vec<u32>,
}

impl Cursor {
    /// The current cursor, `None` without XFixes or on failure.
    pub(super) fn current(x: &X11) -> Option<Self> {
        if !x.ext.xfixes {
            return None;
        }
        let reply = x.conn.xfixes_get_cursor_image().ok()?.reply().ok()?;
        Some(Self {
            position: Point {
                x: i32::from(reply.x),
                y: i32::from(reply.y),
            },
            hotspot: Point {
                x: i32::from(reply.xhot),
                y: i32::from(reply.yhot),
            },
            size: Size::new(u32::from(reply.width), u32::from(reply.height)),
            pixels: reply.cursor_image,
        })
    }

    #[expect(
        clippy::many_single_char_names,
        reason = "x, y, r, g, b are the usual pixel names"
    )]
    /// Draw the cursor into a 4-byte `B, G, R, X|A` image whose top-left
    /// corner is `origin` on the root window. Premultiplied "over".
    pub(super) fn blend_into(&self, bytes: &mut [u8], stride: u32, size: Size, origin: Point) {
        let left = i64::from(self.position.x) - i64::from(self.hotspot.x) - i64::from(origin.x);
        let top = i64::from(self.position.y) - i64::from(self.hotspot.y) - i64::from(origin.y);
        for (cy, row) in (0..i64::from(self.size.height))
            .zip(self.pixels.chunks(self.size.width.max(1) as usize))
        {
            let y = top + cy;
            if y < 0 || y >= i64::from(size.height) {
                continue;
            }
            for (cx, argb) in (0..).zip(row) {
                let px = left + cx;
                if px < 0 || px >= i64::from(size.width) {
                    continue;
                }
                let at = usize::try_from(y * i64::from(stride) + px * 4).unwrap_or(usize::MAX);
                let Some(dst) = bytes.get_mut(at..at + 4) else {
                    continue;
                };
                let [b, g, r, a] = argb.to_le_bytes();
                let keep = 255 - u32::from(a);
                for (d, s) in dst.iter_mut().zip([b, g, r]) {
                    let blended = u32::from(s) + (u32::from(*d) * keep + 127) / 255;
                    *d = u8::try_from(blended.min(255)).unwrap_or(u8::MAX);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn little_endian_depths_map_to_bgr_formats() {
        assert_eq!(pixel_format(24, 32, true), Some(PixelFormat::Bgrx8));
        assert_eq!(pixel_format(32, 32, true), Some(PixelFormat::Bgra8));
        assert_eq!(pixel_format(30, 32, true), Some(PixelFormat::Xr30));
        assert_eq!(
            pixel_format(24, 32, false),
            None,
            "big-endian servers are not modelled"
        );
        assert_eq!(pixel_format(16, 16, true), None);
    }

    #[test]
    fn rows_are_padded_to_the_scanline_pad() {
        assert_eq!(stride(3, 32, 32), 12);
        assert_eq!(stride(3, 8, 32), 4);
        assert_eq!(stride(5, 16, 64), 16);
    }

    #[test]
    fn the_cursor_is_drawn_over_with_premultiplied_alpha() {
        let cursor = Cursor {
            position: Point { x: 11, y: 21 },
            hotspot: Point { x: 1, y: 1 },
            size: Size::new(2, 1),
            // Opaque red, then half-transparent (premultiplied) white.
            pixels: vec![0xffff_0000, 0x8080_8080],
        };
        let mut image = vec![100u8; 4 * 4 * 2];
        cursor.blend_into(&mut image, 16, Size::new(4, 2), Point { x: 10, y: 20 });
        // Cursor top-left lands at (0, 0) of the image.
        assert_eq!(&image[0..3], &[0, 0, 255]);
        assert_eq!(&image[4..7], &[178, 178, 178], "128 + 100 * 127 / 255");
        assert_eq!(&image[8..12], &[100, 100, 100, 100], "outside the cursor");
    }

    #[test]
    fn a_cursor_off_the_image_changes_nothing() {
        let cursor = Cursor {
            position: Point { x: -50, y: -50 },
            hotspot: Point { x: 0, y: 0 },
            size: Size::new(1, 1),
            pixels: vec![0xffff_ffff],
        };
        let mut image = vec![7u8; 16];
        cursor.blend_into(&mut image, 8, Size::new(2, 2), Point { x: 0, y: 0 });
        assert!(image.iter().all(|b| *b == 7));
    }
}
