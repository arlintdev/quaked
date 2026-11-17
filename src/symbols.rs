//! SF Symbols (Apple's icon set, built into macOS) rendered through AppKit into an
//! alpha mask that the board tints. The settings page gets the same icons as PNGs.

use objc2::rc::autoreleasepool;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use std::cell::RefCell;
use std::collections::HashMap;

/// An alpha mask, row-major, `w * h` bytes.
pub struct Mask {
    pub w: u32,
    pub h: u32,
    pub alpha: Vec<u8>,
}

#[derive(Default)]
pub struct Symbols {
    cache: RefCell<HashMap<(String, u32), Option<std::rc::Rc<Mask>>>>,
}

impl Symbols {
    /// The symbol at roughly `px` pixels tall, or None if macOS doesn't know the name.
    pub fn get(&self, name: &str, px: f32) -> Option<std::rc::Rc<Mask>> {
        let key = (name.to_string(), px.round() as u32);
        if let Some(hit) = self.cache.borrow().get(&key) {
            return hit.clone();
        }
        let mask = render(name, px).map(std::rc::Rc::new);
        self.cache.borrow_mut().insert(key, mask.clone());
        mask
    }
}

/// The display name of a running app ("us.zoom.xos" -> "zoom.us"), if it is running.
pub fn running_app_name(bundle_id: &str) -> Option<String> {
    autoreleasepool(|_| unsafe {
        let ns = NSString::from_str(bundle_id);
        let apps: *mut AnyObject = msg_send![class!(NSRunningApplication), runningApplicationsWithBundleIdentifier: &*ns];
        if apps.is_null() {
            return None;
        }
        let count: usize = msg_send![apps, count];
        if count == 0 {
            return None;
        }
        let app: *mut AnyObject = msg_send![apps, objectAtIndex: 0usize];
        let name: *mut AnyObject = msg_send![app, localizedName];
        if name.is_null() {
            return None;
        }
        let utf8: *const std::ffi::c_char = msg_send![name, UTF8String];
        (!utf8.is_null()).then(|| std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned())
    })
}

/// True if macOS has a symbol with this name.
#[allow(dead_code)]
pub fn exists(name: &str) -> bool {
    autoreleasepool(|_| unsafe {
        let ns = NSString::from_str(name);
        let img: *mut AnyObject = msg_send![class!(NSImage), imageWithSystemSymbolName: &*ns, accessibilityDescription: std::ptr::null_mut::<AnyObject>()];
        !img.is_null()
    })
}

fn render(name: &str, px: f32) -> Option<Mask> {
    if name.is_empty() {
        return None;
    }
    autoreleasepool(|_| unsafe {
        let ns = NSString::from_str(name);
        let mut img: *mut AnyObject = msg_send![class!(NSImage), imageWithSystemSymbolName: &*ns, accessibilityDescription: std::ptr::null_mut::<AnyObject>()];
        if img.is_null() {
            return None;
        }
        // Medium weight (NSFontWeightMedium = 0.23) at the requested point size.
        let cfg: *mut AnyObject = msg_send![class!(NSImageSymbolConfiguration), configurationWithPointSize: px as f64, weight: 0.23f64];
        if !cfg.is_null() {
            let configured: *mut AnyObject = msg_send![img, imageWithSymbolConfiguration: cfg];
            if !configured.is_null() {
                img = configured;
            }
        }
        let size: NSSize = msg_send![img, size];
        let (w, h) = (size.width.ceil().max(1.0) as isize, size.height.ceil().max(1.0) as isize);
        let rep_class: &AnyClass = class!(NSBitmapImageRep);
        let rep: *mut AnyObject = msg_send![rep_class, alloc];
        let space = NSString::from_str("NSDeviceRGBColorSpace");
        let rep: *mut AnyObject = msg_send![rep,
            initWithBitmapDataPlanes: std::ptr::null_mut::<*mut u8>(),
            pixelsWide: w,
            pixelsHigh: h,
            bitsPerSample: 8isize,
            samplesPerPixel: 4isize,
            hasAlpha: Bool::YES,
            isPlanar: Bool::NO,
            colorSpaceName: &*space,
            bytesPerRow: 0isize,
            bitsPerPixel: 0isize];
        if rep.is_null() {
            return None;
        }
        let ctx: *mut AnyObject = msg_send![class!(NSGraphicsContext), graphicsContextWithBitmapImageRep: rep];
        if ctx.is_null() {
            let _: () = msg_send![rep, release];
            return None;
        }
        let _: () = msg_send![class!(NSGraphicsContext), saveGraphicsState];
        let _: () = msg_send![class!(NSGraphicsContext), setCurrentContext: ctx];
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w as f64, h as f64));
        let zero = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
        let _: () = msg_send![img, drawInRect: rect, fromRect: zero, operation: 2usize, fraction: 1.0f64];
        let _: () = msg_send![ctx, flushGraphics];
        let _: () = msg_send![class!(NSGraphicsContext), restoreGraphicsState];
        let data: *mut u8 = msg_send![rep, bitmapData];
        let stride: isize = msg_send![rep, bytesPerRow];
        let mut alpha = vec![0u8; (w * h) as usize];
        if !data.is_null() {
            for y in 0..h {
                for x in 0..w {
                    alpha[(y * w + x) as usize] = *data.offset(y * stride + x * 4 + 3);
                }
            }
        }
        let _: () = msg_send![rep, release];
        Some(Mask { w: w as u32, h: h as u32, alpha })
    })
}
