//! What happens to a screenshot after capture: clipboard, file and Finder metadata.

use std::collections::HashSet;
use std::ffi::{CString, c_char, c_int, c_void};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};

use dispatch2::{DispatchQoS, DispatchQueue, GlobalQueueIdentifier};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG};
use objc2_core_foundation::{
    CFData, CFDictionary, CFMutableData, CFNumber, CFRetained, CFString, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::CGColorSpace;
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
    CGInterpolationQuality,
};
use objc2_foundation::{
    NSArray, NSData, NSDate, NSDateFormatter, NSDictionary, NSNumber, NSPropertyListFormat,
    NSPropertyListSerialization, NSString,
};
use objc2_image_io::{
    CGImageDestination, CGImageSource, kCGImageDestinationLossyCompressionQuality, kCGImagePropertyDPIHeight,
    kCGImagePropertyDPIWidth, kCGImagePropertyGIFDelayTime, kCGImagePropertyGIFDictionary,
    kCGImagePropertyGIFLoopCount,
};

use crate::capture::{Kind, Shot};
use crate::settings::{AfterCapture, DEFAULT_FILE_NAME_TEMPLATE, Format, Settings, WindowBackground};
use crate::{backdrop, bridge, editor, library, pin, preview, recognize};

unsafe extern "C" {
    fn setxattr(
        path: *const c_char,
        name: *const c_char,
        value: *const c_void,
        size: usize,
        position: u32,
        options: c_int,
    ) -> c_int;
}

/// Where the next area screenshot goes instead of the usual output.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Route {
    Normal,
    /// To the canvas of the editor waiting for it.
    Editor,
    /// To text recognition only.
    Text,
    /// The selected area is scrolled and stitched instead of captured right away.
    Scroll,
}

/// Route of the next screenshot. Set on the main thread, read on the capture thread.
static ROUTE: AtomicU8 = AtomicU8::new(0);

/// Sends the next area screenshot past the clipboard, file and preview.
pub fn route_next(route: Route) {
    ROUTE.store(route as u8, Ordering::SeqCst);
}

/// The selection was cancelled, the next screenshot goes the usual way.
pub fn cancel_route() {
    route_next(Route::Normal);
}

/// Takes the scroll route if the selection was for it. Other routes stay in place.
pub fn take_scroll_route() -> bool {
    ROUTE.compare_exchange(Route::Scroll as u8, Route::Normal as u8, Ordering::SeqCst, Ordering::SeqCst).is_ok()
}

fn take_route() -> Route {
    match ROUTE.swap(Route::Normal as u8, Ordering::SeqCst) {
        1 => Route::Editor,
        2 => Route::Text,
        3 => Route::Scroll,
        _ => Route::Normal,
    }
}

/// Delivers a screenshot: adds the background behind a window if one is chosen, then copies, saves and shows it.
pub fn deliver(image: &CGImage, scale: f64, shot: &Shot) {
    // The window is captured with a margin, the empty space around it is unwanted in every mode.
    let cropped = if shot.kind == Kind::Window { backdrop::crop_transparent(image) } else { None };
    let image = cropped.as_deref().unwrap_or(image);
    // A screenshot for the editor or text recognition is not copied, saved or made the last one.
    match take_route() {
        Route::Editor => {
            let image = retain(image);
            return DispatchQueue::main().exec_async(move || editor::place_capture(image, scale));
        }
        Route::Text => return recognize::recognize_and_show(retain(image)),
        Route::Normal | Route::Scroll => {}
    }
    let settings = Settings::load();
    let background = settings.window_background;
    if shot.kind == Kind::Window && matches!(background, WindowBackground::Wallpaper | WindowBackground::Color) {
        // The screen wallpaper is looked up on the main thread, drawing and output go back to the background.
        let (image, shot, color) = (retain(image), shot.clone(), settings.window_color);
        return DispatchQueue::main().exec_async(move || {
            let Some(mtm) = MainThreadMarker::new() else { return };
            let wallpaper =
                if background == WindowBackground::Wallpaper { backdrop::wallpaper(mtm, &shot) } else { None };
            in_background(move || {
                let framed = backdrop::behind_window(&image, scale, shot.global_rect, wallpaper.as_ref(), color);
                finish(framed.as_deref().unwrap_or(&image), scale, &shot);
            });
        });
    }
    if shot.kind == Kind::Window && background == WindowBackground::Clean {
        // With transparent corners a screenshot on a light background gets white wedges around the rounding.
        let squared = backdrop::square_corners(image, scale);
        return finish(squared.as_deref().unwrap_or(image), scale, shot);
    }
    finish(image, scale, shot);
}

/// Copies and saves the finished screenshot, then shows the preview or a confirmation.
fn finish(image: &CGImage, scale: f64, shot: &Shot) {
    let settings = Settings::load();
    let png = if settings.copy_to_clipboard { copy(image, scale) } else { None };
    let saved = if settings.save_to_file {
        save(&settings.directory, image, scale, Some(shot), png.as_deref())
            .inspect_err(|err| eprintln!("kadro: failed to save screenshot: {err}"))
            .ok()
    } else {
        None
    };

    pin::remember(image, scale, png.as_deref());

    let copied = png.is_some();
    let image = retain(image);
    let shot = shot.clone();
    let after_capture = settings.after_capture;
    DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else { return };
        pin::remember_place(shot.clone());
        match after_capture {
            AfterCapture::Preview => preview::show(mtm, image, scale, shot, copied || saved.is_some()),
            AfterCapture::Editor => editor::open(mtm, image, scale, Some(shot), "Screenshot"),
            AfterCapture::Nothing => preview::confirm(mtm, copied, saved.as_deref(), &shot),
        }
    });
}

/// Runs file work and encoding off the main thread so the interface does not freeze.
pub fn in_background(work: impl FnOnce() + Send + 'static) {
    DispatchQueue::global_queue(GlobalQueueIdentifier::QualityOfService(DispatchQoS::UserInitiated)).exec_async(work);
}

/// Encodes the screenshot and puts it on the clipboard in the background.
pub fn copy_in_background(image: CFRetained<CGImage>, scale: f64) {
    in_background(move || {
        copy(&image, scale);
    });
}

/// Puts the screenshot on the clipboard as PNG. Returns the encoded bytes so they are not encoded twice.
pub fn copy(image: &CGImage, scale: f64) -> Option<Vec<u8>> {
    let Some(png) = encode(image, Format::Png, scale) else {
        eprintln!("kadro: failed to encode clipboard image");
        return None;
    };
    let bytes = png.clone();
    // NSPasteboard is safe only on the main thread.
    DispatchQueue::main().exec_async(move || copy_to_clipboard(&bytes));
    Some(png)
}

/// Saves the screenshot to the folder per the format, downscale and file name template settings.
/// Without `shot` the image was opened from a file or the clipboard rather than captured, and gets no screenshot tags.
pub fn save(
    directory: &Path,
    image: &CGImage,
    scale: f64,
    shot: Option<&Shot>,
    png: Option<&[u8]>,
) -> std::io::Result<PathBuf> {
    let path = save_unindexed(directory, image, scale, shot, png)?;
    library::add(&path);
    Ok(path)
}

/// Saves like [`save`], but keeps the file out of the library.
pub fn save_unindexed(
    directory: &Path,
    image: &CGImage,
    scale: f64,
    shot: Option<&Shot>,
    png: Option<&[u8]>,
) -> std::io::Result<PathBuf> {
    let (bytes, name, format) = encoded(image, scale, shot, png)?;
    std::fs::create_dir_all(directory)?;
    let (path, mut file) = create_unique(directory, &name, format.extension())?;
    file.write_all(&bytes)?;
    if let Some(shot) = shot {
        mark_as_screenshot(&path, shot);
    }
    Ok(path)
}

/// Screenshot in the format from settings and the file name from the template, without extension.
/// `png` is the already encoded PNG, if there is one.
pub fn encoded(
    image: &CGImage,
    scale: f64,
    shot: Option<&Shot>,
    png: Option<&[u8]>,
) -> std::io::Result<(Vec<u8>, String, Format)> {
    let settings = Settings::load();
    let downscaled = (settings.downscale_retina && scale > 1.0).then(|| downscale(image, scale)).flatten();
    let format = match settings.format {
        Format::Auto if looks_like_photo(image) => Format::Jpeg,
        Format::Auto => Format::Png,
        format => format,
    };
    let bytes = match (&downscaled, png) {
        // The same screenshot in the same format is already encoded for the clipboard.
        (None, Some(png)) if format == Format::Png => Some(png.to_vec()),
        (Some(small), _) => encode(small, format, 1.0),
        (None, _) => encode(image, format, scale),
    };
    let bytes = bytes.ok_or_else(|| std::io::Error::other(format!("{} encoding failed", format.extension())))?;
    let (date, time) = (now("yyyy-MM-dd"), now("HH.mm.ss"));
    let (mode, app) = shot.map_or(("Image", ""), |shot| (shot.kind.title(), shot.app.as_str()));
    Ok((bytes, file_name(&settings.file_name_template, &date, &time, mode, app), format))
}

/// Writes the image to the chosen path. The format comes from the extension, an unknown extension is an error.
pub fn write_to(path: &Path, image: &CGImage, scale: f64) -> std::io::Result<()> {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
    let format = match extension.as_str() {
        "png" => Format::Png,
        "jpg" | "jpeg" => Format::Jpeg,
        "heic" => Format::Heic,
        _ => return Err(std::io::Error::other(format!("unsupported file type .{extension}"))),
    };
    let bytes = encode(image, format, scale).ok_or_else(|| std::io::Error::other("encoding failed"))?;
    std::fs::write(path, bytes)
}

/// Decodes an image and its scale from DPI. Without DPI a pixel is assumed to equal a point.
pub fn decode(bytes: &[u8]) -> Option<(CFRetained<CGImage>, f64)> {
    let source = unsafe { CGImageSource::with_data(&CFData::from_bytes(bytes), None) }?;
    let image = unsafe { source.image_at_index(0, None) }?;
    let dpi = unsafe { source.properties_at_index(0, None) }.and_then(|properties| {
        // SAFETY: image properties are a dictionary with string keys, CFDictionary and NSDictionary are toll-free bridged.
        let properties: &NSDictionary<NSString, AnyObject> = unsafe { bridge::toll_free(&*properties) };
        // SAFETY: the key is an ImageIO CFString constant, and CFString and NSString are toll-free bridged.
        let key: &NSString = unsafe { bridge::toll_free(kCGImagePropertyDPIWidth) };
        bridge::number(properties, key).map(|n| n.doubleValue())
    });
    Some((image, (dpi.unwrap_or(72.0) / 72.0).max(1.0)))
}

pub fn encode_png(image: &CGImage, scale: f64) -> Option<Vec<u8>> {
    encode(image, Format::Png, scale)
}

pub fn retain(image: &CGImage) -> CFRetained<CGImage> {
    unsafe { CFRetained::retain(std::ptr::NonNull::from(image)) }
}

/// Encodes the screenshot to bytes of the chosen format.
fn encode(image: &CGImage, format: Format, scale: f64) -> Option<Vec<u8>> {
    let data = CFMutableData::new(None, 0)?;
    let destination =
        unsafe { CGImageDestination::with_data(&data, &CFString::from_static_str(format.uti()), 1, None) }?;
    // DPI 144 on a Retina screenshot makes Preview and browsers show it at point size, not pixel size.
    let dpi = CFNumber::new_f64(72.0 * scale);
    let quality = CFNumber::new_f64(0.9);
    let keys =
        unsafe { [kCGImagePropertyDPIWidth, kCGImagePropertyDPIHeight, kCGImageDestinationLossyCompressionQuality] };
    let properties = CFDictionary::<CFString, CFNumber>::from_slices(&keys, &[&dpi, &dpi, &quality]);
    unsafe { destination.add_image(image, Some(properties.as_ref())) };
    unsafe { destination.finalize() }.then(|| data.to_vec())
}

/// Animated GIF of frames that loop, switching every `seconds`.
pub fn encode_gif(frames: &[&CGImage], seconds: f64) -> Option<Vec<u8>> {
    let data = CFMutableData::new(None, 0)?;
    let gif = CFString::from_static_str("com.compuserve.gif");
    let destination = unsafe { CGImageDestination::with_data(&data, &gif, frames.len(), None) }?;
    // SAFETY: the keys are ImageIO constants, they live for the whole process.
    let (dictionary, loop_count, delay) =
        unsafe { (kCGImagePropertyGIFDictionary, kCGImagePropertyGIFLoopCount, kCGImagePropertyGIFDelayTime) };
    let gif_properties = |key: &CFString, value: &CFNumber| {
        let inner = CFDictionary::<CFString, CFNumber>::from_slices(&[key], &[value]);
        CFDictionary::<CFString, CFDictionary<CFString, CFNumber>>::from_slices(&[dictionary], &[&inner])
    };
    // 0 repeats means an endless loop.
    unsafe { destination.set_properties(Some(gif_properties(loop_count, &CFNumber::new_i32(0)).as_ref())) };
    let frame = gif_properties(delay, &CFNumber::new_f64(seconds));
    for image in frames {
        unsafe { destination.add_image(image, Some(frame.as_ref())) };
    }
    unsafe { destination.finalize() }.then(|| data.to_vec())
}

/// Scales the screenshot down to a size in points. Returns None if the context could not be created.
fn downscale(image: &CGImage, scale: f64) -> Option<CFRetained<CGImage>> {
    let width = (CGImage::width(Some(image)) as f64 / scale).round() as usize;
    let height = (CGImage::height(Some(image)) as f64 / scale).round() as usize;
    let space = CGImage::color_space(Some(image));
    let context = unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            width,
            height,
            8,
            0,
            space.as_deref(),
            CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
        )
    }?;
    CGContext::set_interpolation_quality(Some(&context), CGInterpolationQuality::High);
    let bounds = CGRect::new(CGPoint::ZERO, CGSize::new(width as f64, height as f64));
    CGContext::draw_image(Some(&context), bounds, Some(image));
    CGBitmapContextCreateImage(Some(&context))
}

fn copy_to_clipboard(png: &[u8]) {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    pasteboard.setData_forType(Some(&NSData::with_bytes(png)), unsafe { NSPasteboardTypePNG });
}

fn now(format: &str) -> String {
    let formatter = NSDateFormatter::new();
    formatter.setDateFormat(Some(&NSString::from_str(format)));
    formatter.stringFromDate(&NSDate::now()).to_string()
}

/// File name from the template with {date}, {time}, {mode} and {app} placeholders.
fn file_name(template: &str, date: &str, time: &str, mode: &str, app: &str) -> String {
    let render = |template: &str| {
        template
            .replace("{date}", date)
            .replace("{time}", time)
            .replace("{mode}", mode)
            .replace("{app}", app)
            // "/" splits the path, and Finder shows ":" as "/".
            .replace(['/', ':'], "-")
            .trim()
            .to_string()
    };
    let name = render(template);
    // Finder hides or refuses a name of only spaces or one that starts with a dot.
    if name.is_empty() || name.starts_with('.') { render(DEFAULT_FILE_NAME_TEMPLATE) } else { name }
}

/// Creates a new file under a free name, adding a number on collision.
fn create_unique(directory: &Path, name: &str, extension: &str) -> std::io::Result<(PathBuf, File)> {
    let mut path = directory.join(format!("{name}.{extension}"));
    let mut n = 2;
    loop {
        // create_new fails on any existing name, a dangling symlink included. Otherwise another account
        // could plant a link under the next screenshot name in a shared folder and have the write go to its target.
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                path = directory.join(format!("{name} ({n}).{extension}"));
                n += 1;
            }
            result => return result.map(|file| (path, file)),
        }
    }
}

/// Marks the file as a screenshot, like the system screencapture does,
/// so Finder and Spotlight find it among screenshots. Errors are only logged.
fn mark_as_screenshot(path: &Path, shot: &Shot) {
    let r = shot.global_rect;
    let rect = NSArray::from_retained_slice(&[
        NSNumber::new_f64(r.origin.x),
        NSNumber::new_f64(r.origin.y),
        NSNumber::new_f64(r.size.width),
        NSNumber::new_f64(r.size.height),
    ]);
    let attributes: [(&str, Retained<AnyObject>); 3] = [
        ("com.apple.metadata:kMDItemIsScreenCapture", NSNumber::new_bool(true).into()),
        ("com.apple.metadata:kMDItemScreenCaptureType", NSString::from_str(shot.kind.finder_type()).into()),
        ("com.apple.metadata:kMDItemScreenCaptureGlobalRect", rect.into()),
    ];
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else { return };
    for (name, value) in attributes {
        let data = unsafe {
            NSPropertyListSerialization::dataWithPropertyList_format_options_error(
                &value,
                NSPropertyListFormat::BinaryFormat_v1_0,
                0,
            )
        };
        let (Ok(data), Ok(c_name)) = (data, CString::new(name)) else { continue };
        let bytes = data.to_vec();
        let status = unsafe { setxattr(c_path.as_ptr(), c_name.as_ptr(), bytes.as_ptr().cast(), bytes.len(), 0, 0) };
        if status != 0 {
            eprintln!("kadro: failed to set {name}: {}", std::io::Error::last_os_error());
        }
    }
}

/// Whether the screenshot looks like a photo or a gradient that suits JPEG better.
fn looks_like_photo(image: &CGImage) -> bool {
    const SIDE: usize = 160;
    let (width, height) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let ratio = (SIDE as f64 / width.max(height) as f64).min(1.0);
    let (w, h) = (((width as f64 * ratio) as usize).max(1), ((height as f64 * ratio) as usize).max(1));
    let mut pixels = vec![0u32; w * h];
    let space = CGColorSpace::new_device_rgb();
    let Some(context) = (unsafe {
        CGBitmapContextCreate(
            pixels.as_mut_ptr().cast(),
            w,
            h,
            8,
            w * 4,
            space.as_deref(),
            CGImageAlphaInfo::NoneSkipFirst.0 | CGImageByteOrderInfo::Order32Little.0,
        )
    }) else {
        return false;
    };
    // No smoothing: blending pixels while scaling down adds extra colors even to text.
    CGContext::set_interpolation_quality(Some(&context), CGInterpolationQuality::None);
    CGContext::draw_image(Some(&context), CGRect::new(CGPoint::ZERO, CGSize::new(w as f64, h as f64)), Some(image));
    drop(context);
    is_photographic(&pixels)
}

/// Interfaces and text have few distinct colors, in photos and noise almost every pixel is unique.
fn is_photographic(pixels: &[u32]) -> bool {
    let unique: HashSet<_> = pixels.iter().map(|p| p & 0x00ff_ffff).collect();
    unique.len() * 4 > pixels.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_unique_skips_taken_names() {
        let dir = std::env::temp_dir().join(format!("kadro-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (first, _) = create_unique(&dir, "Kadro t", "png").unwrap();
        assert_eq!(first, dir.join("Kadro t.png"));
        assert_eq!(create_unique(&dir, "Kadro t", "png").unwrap().0, dir.join("Kadro t (2).png"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn create_unique_does_not_write_through_a_planted_symlink() {
        let dir = std::env::temp_dir().join(format!("kadro-symlink-{}", std::process::id()));
        let target = dir.join("victim.txt");
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(&target, dir.join("Kadro t.png")).unwrap();
        let (path, mut file) = create_unique(&dir, "Kadro t", "png").unwrap();
        file.write_all(b"png").unwrap();
        assert_eq!(path, dir.join("Kadro t (2).png"));
        assert!(!target.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn file_name_fills_tokens_and_falls_back_to_default() {
        assert_eq!(
            file_name("{app} {mode} {date}", "2026-11-03", "14.12.00", "Window", "Notes/Work"),
            "Notes-Work Window 2026-11-03"
        );
        assert_eq!(
            file_name("Kadro {date} at {time}", "2026-11-03", "14.12.00", "Window", ""),
            "Kadro 2026-11-03 at 14.12.00"
        );
        // an empty app gives an empty name
        assert_eq!(file_name("{app}", "d", "t", "Window", ""), "Kadro d at t");
        assert_eq!(file_name(".hidden", "d", "t", "Window", ""), "Kadro d at t");
    }

    #[test]
    fn gif_has_both_frames_and_loops_forever() {
        let space = CGColorSpace::new_device_rgb();
        let frame = |gray: f64| {
            // SAFETY: a null buffer makes Core Graphics allocate it.
            let context = unsafe {
                CGBitmapContextCreate(
                    std::ptr::null_mut(),
                    6,
                    4,
                    8,
                    0,
                    space.as_deref(),
                    CGImageAlphaInfo::PremultipliedLast.0,
                )
            }
            .unwrap();
            CGContext::set_rgb_fill_color(Some(&context), gray, gray, gray, 1.0);
            CGContext::fill_rect(Some(&context), CGRect::new(CGPoint::ZERO, CGSize::new(6.0, 4.0)));
            CGBitmapContextCreateImage(Some(&context)).unwrap()
        };
        let (dark, light) = (frame(0.1), frame(0.9));
        let gif = encode_gif(&[&dark, &light], 1.0).unwrap();
        // ImageIO writes a GIF87a header, but the loop and delay extensions are in place
        assert_eq!(&gif[..3], b"GIF");
        let source = unsafe { CGImageSource::with_data(&CFData::from_bytes(&gif), None) }.unwrap();
        assert_eq!(unsafe { source.count() }, 2);
        // NETSCAPE2.0 with zero repeats is an endless loop, as Safari and Preview read it
        let netscape = gif.windows(11).position(|w| w == b"NETSCAPE2.0").expect("loop extension");
        assert_eq!(&gif[netscape + 13..netscape + 15], &[0, 0]);
        // every frame has a delay of 100 hundredths of a second
        let delays = gif.windows(6).filter(|w| w[..3] == [0x21, 0xf9, 0x04] && w[4..] == [100, 0]).count();
        assert_eq!(delays, 2);
    }

    #[test]
    fn write_to_picks_the_format_from_the_extension() {
        let space = CGColorSpace::new_device_rgb();
        // SAFETY: a null buffer makes Core Graphics allocate it.
        let context = unsafe {
            CGBitmapContextCreate(
                std::ptr::null_mut(),
                4,
                2,
                8,
                0,
                space.as_deref(),
                CGImageAlphaInfo::PremultipliedLast.0,
            )
        }
        .unwrap();
        let image = CGBitmapContextCreateImage(Some(&context)).unwrap();
        let dir = std::env::temp_dir().join(format!("kadro-write-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write_to(&dir.join("a.JPG"), &image, 2.0).unwrap();
        let (decoded, scale) = decode(&std::fs::read(dir.join("a.JPG")).unwrap()).unwrap();
        assert_eq!((CGImage::width(Some(&decoded)), scale), (4, 2.0));
        assert!(write_to(&dir.join("a.bmp"), &image, 1.0).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn flat_interface_is_png_and_noise_is_photo() {
        let interface: Vec<u32> = (0..25_600).map(|i| [0xffffff, 0x1e1e1e, 0x3478f6, 0x888888][i % 4]).collect();
        assert!(!is_photographic(&interface));
        // smooth horizontal gradient, one color per column
        let gradient: Vec<u32> = (0..25_600u32).map(|i| (i % 160) * 0x010101).collect();
        assert!(!is_photographic(&gradient));
        let mut seed = 1u32;
        let noise: Vec<u32> = (0..25_600)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                seed >> 8
            })
            .collect();
        assert!(is_photographic(&noise));
    }
}
