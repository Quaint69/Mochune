//! Local alpha-composited UI only. No decoded video pixels enter this surface.
//! Unlike holes in the native video window, alpha preserves rounded loaders,
//! translucent controls and letterbox bars without stale swapchain pixels.
use super::{
    native_overlay::Overlay,
    process_protocol::Input,
    windows_native::{Inputs, child_proc},
};
use crate::SetupWindow;
use anyhow::{Context, Result};
use slint::ComponentHandle;
use std::{ffi::c_void, ptr};
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, POINT, SIZE},
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
    core::w,
};

struct Bitmap {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    size: [i32; 2],
}
impl Bitmap {
    fn new(size: [i32; 2]) -> Result<Self> {
        let dc = unsafe { CreateCompatibleDC(None) };
        anyhow::ensure!(!dc.0.is_null(), "Create controls memory DC");
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size[0],
                biHeight: -size[1],
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = ptr::null_mut();
        let bitmap = match unsafe {
            CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
        } {
            Ok(bitmap) => bitmap,
            Err(error) => {
                unsafe {
                    let _ = DeleteDC(dc);
                }
                return Err(error.into());
            }
        };
        let previous = unsafe { SelectObject(dc, HGDIOBJ(bitmap.0)) };
        Ok(Self {
            dc,
            bitmap,
            previous,
            bits: bits.cast(),
            size,
        })
    }
}
impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}
pub(super) struct Layer {
    pub ui: Overlay,
    hwnd: HWND,
    owner: HWND,
    inputs: Box<Inputs>,
    bitmap: Option<Bitmap>,
    // Physical coordinates relative to the native video host for input routing.
    origin: [i32; 2],
    last: Option<(u64, [i32; 8])>,
    corner_shape: Option<[i32; 8]>,
    shown: bool,
    pub uploads: u64,
}
impl Layer {
    pub fn new(owner: HWND, main: &SetupWindow) -> Result<Self> {
        let ui = Overlay::new(Some(main.as_weak()))?;
        let instance = unsafe { GetModuleHandleW(None) }?;
        static CLASS: std::sync::OnceLock<std::result::Result<(), String>> =
            std::sync::OnceLock::new();
        CLASS
            .get_or_init(|| {
                let class = WNDCLASSW {
                    lpfnWndProc: Some(child_proc),
                    hInstance: HINSTANCE(instance.0),
                    lpszClassName: w!("MochuneVideoControls"),
                    hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
                    ..Default::default()
                };
                if unsafe { RegisterClassW(&class) } == 0 {
                    Err(windows::core::Error::from_thread().to_string())
                } else {
                    Ok(())
                }
            })
            .as_ref()
            .map_err(|e| anyhow::anyhow!("Register controls window: {e}"))?;
        let inputs = Box::new(Inputs::default());
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                w!("MochuneVideoControls"),
                w!("Mochune video controls"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                Some(owner),
                None,
                Some(HINSTANCE(instance.0)),
                Some((&*inputs as *const Inputs).cast::<c_void>()),
            )
        }?;
        Ok(Self {
            ui,
            hwnd,
            owner,
            inputs,
            bitmap: None,
            origin: [0; 2],
            last: None,
            corner_shape: None,
            shown: false,
            uploads: 0,
        })
    }
    pub fn inputs(&self) -> Vec<Input> {
        self.inputs
            .events
            .lock()
            .unwrap()
            .drain(..)
            .map(|event| match event {
                Input::Move(x, y) => Input::Move(x + self.origin[0], y + self.origin[1]),
                Input::Down(x, y) => Input::Down(x + self.origin[0], y + self.origin[1]),
                Input::Up(x, y) => Input::Up(x + self.origin[0], y + self.origin[1]),
                other => other,
            })
            .collect()
    }
    pub fn hide(&mut self) {
        if self.shown {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.shown = false;
        }
        self.last = None;
    }
    pub fn bytes(&self) -> usize {
        self.bitmap
            .as_ref()
            .map_or(0, |b| (b.size[0] * b.size[1]) as usize * 4)
    }
    pub fn present(&mut self, geo: [i32; 8], host_shown: bool) -> Result<()> {
        let foreground = unsafe { GetAncestor(GetForegroundWindow(), GA_ROOTOWNER) };
        if !host_shown || foreground != self.owner || unsafe { IsIconic(self.owner) }.as_bool() {
            self.hide();
            return Ok(());
        }
        let Some(art) = self.ui.artwork.clone() else {
            self.hide();
            return Ok(());
        };
        if art.canvas.contains(&0) {
            self.hide();
            return Ok(());
        }
        let sx = geo[2] as f64 / art.canvas[0] as f64;
        let sy = geo[3] as f64 / art.canvas[1] as f64;
        let x = ((art.rect[0] as f64 * sx).floor() as i32).max(geo[4]);
        let y = ((art.rect[1] as f64 * sy).floor() as i32).max(geo[5]);
        let right = (((art.rect[0] + art.rect[2]) as f64 * sx).ceil() as i32).min(geo[6]);
        let bottom = (((art.rect[1] + art.rect[3]) as f64 * sy).ceil() as i32).min(geo[7]);
        let size = [right - x, bottom - y];
        if size[0] <= 0 || size[1] <= 0 {
            self.hide();
            return Ok(());
        }
        // Bound UI allocation independently of display DPI. Refuse impossible
        // dimensions rather than ever allocating an unbounded pixel buffer.
        anyhow::ensure!(
            (size[0] as u64) * (size[1] as u64) <= 16_777_216,
            "Controls surface exceeds size limit"
        );
        let mut screen = POINT {
            x: geo[0] + x,
            y: geo[1] + y,
        };
        unsafe { ClientToScreen(self.owner, &mut screen) }
            .ok()
            .context("Position video controls")?;
        let inset = (f64::from(self.ui.view.get_picture_border_width()) * sx.min(sy))
            .round()
            .max(0.) as i32;
        let radius = ((f64::from(self.ui.view.get_picture_radius()) * sx.min(sy)).round() as i32
            - inset)
            .max(0);
        let signature = (
            art.version,
            [screen.x, screen.y, size[0], size[1], x, y, radius, inset],
        );
        if self.last == Some(signature) && self.shown {
            return Ok(());
        }
        if self.bitmap.as_ref().is_none_or(|b| b.size != size) {
            self.bitmap = Some(Bitmap::new(size)?);
        }
        let b = self.bitmap.as_ref().unwrap();
        let destination = unsafe { std::slice::from_raw_parts_mut(b.bits, self.bytes()) };
        // Resample UI artwork only, preserving premultiplied alpha at every DPI.
        for dy in 0..size[1] as usize {
            let ay = (((y as f64 + dy as f64) / sy).floor() as u32)
                .saturating_sub(art.rect[1])
                .min(art.rect[3] - 1);
            for dx in 0..size[0] as usize {
                let ax = (((x as f64 + dx as f64) / sx).floor() as u32)
                    .saturating_sub(art.rect[0])
                    .min(art.rect[2] - 1);
                let source = ((ay * art.rect[2] + ax) * 4) as usize;
                let target = (dy * size[0] as usize + dx) * 4;
                destination[target..target + 4].copy_from_slice(&art.bgra[source..source + 4]);
            }
        }
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&screen),
                Some(&SIZE {
                    cx: size[0],
                    cy: size[1],
                }),
                Some(b.dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
        .context("Composite video controls")?;
        // Translate the full picture's rounded region into this small crop.
        // No full-frame UI bitmap or decoded-frame readback is needed.
        let shape = [geo[2], geo[3], x, y, size[0], size[1], radius, inset];
        if self.corner_shape != Some(shape) {
            if radius > 0 || inset > 0 {
                let region = unsafe {
                    CreateRoundRectRgn(
                        inset - x,
                        inset - y,
                        geo[2] - inset - x + i32::from(inset == 0),
                        geo[3] - inset - y + i32::from(inset == 0),
                        radius * 2,
                        radius * 2,
                    )
                };
                if unsafe { SetWindowRgn(self.hwnd, Some(region), true) } == 0 {
                    let _ = unsafe { DeleteObject(HGDIOBJ(region.0)) };
                }
            } else {
                unsafe {
                    SetWindowRgn(self.hwnd, None, true);
                }
            }
            self.corner_shape = Some(shape);
        }
        self.origin = [x, y];
        self.last = Some(signature);
        self.uploads += 1;
        if !self.shown {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            self.shown = true;
        }
        Ok(())
    }
}
impl Drop for Layer {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.hwnd).ok();
        }
    }
}
