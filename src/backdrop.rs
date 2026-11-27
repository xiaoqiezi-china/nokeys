//! Desktop backdrop capture + blur, used for the frosted-glass wheel.
//!
//! Slint has no true `backdrop-filter` for the desktop behind a transparent window,
//! and Windows' acrylic applies to the whole window rect (which would frost the
//! corners outside the disc). So we snapshot the screen region under the overlay,
//! blur it ourselves, and paint the result inside the disc.


use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    SetStretchBltMode, StretchBlt, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT,
    DIB_RGB_COLORS, HALFTONE, HGDIOBJ, SRCCOPY,
};

/// A captured screen region as RGBA8, top-down.
pub struct Backdrop {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Separable box blur, repeated 3 times to approximate a gaussian.
/// `radius` is in physical pixels.
pub fn blur(backdrop: &mut Backdrop, radius: u32) {
    let r = radius as i32;
    if r <= 0 {
        return;
    }
    let w = backdrop.width as usize;
    let h = backdrop.height as usize;
    let mut buf = backdrop.pixels.clone();
    for pass in 0..3 {
        // alternate pass order to reduce directional artifacts
        if pass % 2 == 0 {
            box_blur_h(&buf, &mut backdrop.pixels, w, h, r);
            box_blur_v(&backdrop.pixels, &mut buf, w, h, r);
        } else {
            box_blur_v(&buf, &mut backdrop.pixels, w, h, r);
            box_blur_h(&backdrop.pixels, &mut buf, w, h, r);
        }
    }
    backdrop.pixels = buf;
}

fn box_blur_h(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: i32) {
    let win = (2 * r + 1) as u32;
    for y in 0..h {
        let row = y * w * 4;
        let mut sum = [0u32; 3];
        // initial window
        for x in -r..=r {
            let cx = x.clamp(0, w as i32 - 1) as usize;
            for c in 0..3 {
                sum[c] += src[row + cx * 4 + c] as u32;
            }
        }
        for x in 0..w {
            let o = row + x * 4;
            for c in 0..3 {
                dst[o + c] = (sum[c] / win) as u8;
            }
            dst[o + 3] = 255;
            let out_x = (x as i32 - r).clamp(0, w as i32 - 1) as usize;
            let in_x = (x as i32 + r + 1).clamp(0, w as i32 - 1) as usize;
            for c in 0..3 {
                sum[c] = sum[c] + src[row + in_x * 4 + c] as u32 - src[row + out_x * 4 + c] as u32;
            }
        }
    }
}

fn box_blur_v(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: i32) {
    let win = (2 * r + 1) as u32;
    for x in 0..w {
        let col = x * 4;
        let mut sum = [0u32; 3];
        for y in -r..=r {
            let cy = y.clamp(0, h as i32 - 1) as usize;
            for c in 0..3 {
                sum[c] += src[cy * w * 4 + col + c] as u32;
            }
        }
        for y in 0..h {
            let o = y * w * 4 + col;
            for c in 0..3 {
                dst[o + c] = (sum[c] / win) as u8;
            }
            dst[o + 3] = 255;
            let out_y = (y as i32 - r).clamp(0, h as i32 - 1) as usize;
            let in_y = (y as i32 + r + 1).clamp(0, h as i32 - 1) as usize;
            for c in 0..3 {
                sum[c] = sum[c] + src[in_y * w * 4 + col + c] as u32
                    - src[out_y * w * 4 + col + c] as u32;
            }
        }
    }
}

/// Capture the screen rectangle shrunk by `div` in each direction.
///
/// The frost is a blur anyway, so the lost detail never shows; the blur cost
/// drops with the square of the divisor, which is what makes a live backdrop
/// affordable.
pub fn capture_shrunk(x: i32, y: i32, w: i32, h: i32, div: i32) -> Option<Backdrop> {
    let div = div.max(1);
    let (tw, th) = ((w / div).max(1), (h / div).max(1));
    if w <= 0 || h <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        if screen.0.is_null() {
            return None;
        }
        let mem = CreateCompatibleDC(Some(screen));
        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: tw,
            biHeight: -th,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let Ok(dib) = CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            return None;
        };
        if dib.0.is_null() || bits.is_null() {
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            return None;
        }
        let old = SelectObject(mem, HGDIOBJ(dib.0));
        SetStretchBltMode(mem, HALFTONE);
        let blt =
            StretchBlt(mem, 0, 0, tw, th, Some(screen), x, y, w, h, SRCCOPY | CAPTUREBLT).as_bool();
        SelectObject(mem, old);

        let mut result = None;
        if blt {
            let len = (tw as usize) * (th as usize) * 4;
            let src = std::slice::from_raw_parts(bits as *const u8, len);
            let mut dst = vec![0u8; len];
            for i in (0..len).step_by(4) {
                dst[i] = src[i + 2];
                dst[i + 1] = src[i + 1];
                dst[i + 2] = src[i];
                dst[i + 3] = 255;
            }
            result = Some(Backdrop {
                width: tw as u32,
                height: th as u32,
                pixels: dst,
            });
        }
        let _ = DeleteObject(HGDIOBJ(dib.0));
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        result
    }
}

/// True when the frame carries no light at all, which means the capture missed
/// the content (exclusive fullscreen, protected output) rather than a dark scene.
pub fn is_blank(backdrop: &Backdrop) -> bool {
    let step = (backdrop.pixels.len() / 4 / 256).max(1) * 4;
    backdrop
        .pixels
        .chunks(step)
        .all(|px| px.len() < 3 || (px[0] == 0 && px[1] == 0 && px[2] == 0))
}
