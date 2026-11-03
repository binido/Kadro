use std::collections::HashSet;
use std::ffi::{c_char, c_int, c_void, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use dispatch2::DispatchQueue;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG};
use objc2_core_foundation::{CFDictionary, CFMutableData, CFNumber, CFRetained, CFString, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGContext, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, CGInterpolationQuality,
};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::MainThreadMarker;
use objc2_core_graphics::CGColorSpace;
use objc2_foundation::{
    NSArray, NSData, NSDate, NSDateFormatter, NSNumber, NSPropertyListFormat, NSPropertyListSerialization,
    NSString,
};
use objc2_image_io::{
    kCGImageDestinationLossyCompressionQuality, kCGImagePropertyDPIHeight, kCGImagePropertyDPIWidth,
    CGImageDestination,
};

use crate::capture::Shot;
use crate::preview;
use crate::settings::{Format, Settings, DEFAULT_FILE_NAME_TEMPLATE};

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

/// Отдаёт снимок в буфер обмена и в файл согласно настройкам, затем показывает превью или подтверждение.
pub fn deliver(image: &CGImage, scale: f64, shot: &Shot) {
    let settings = Settings::load();
    let png = if settings.copy_to_clipboard { copy(image, scale) } else { None };
    let saved = if settings.save_to_file {
        save(&settings.directory, image, scale, shot, png.as_deref())
            .inspect_err(|err| eprintln!("kadro: failed to save screenshot: {err}"))
            .ok()
    } else {
        None
    };

    let copied = png.is_some();
    let image = retain(image);
    let shot = shot.clone();
    let show_preview = settings.show_preview;
    DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else { return };
        if show_preview {
            preview::show(mtm, image, scale, shot, copied || saved.is_some());
        } else {
            preview::confirm(mtm, copied, saved.as_deref(), &shot);
        }
    });
}

/// Кладёт снимок в буфер обмена как PNG. Возвращает закодированные байты, чтобы их не кодировать второй раз.
pub fn copy(image: &CGImage, scale: f64) -> Option<Vec<u8>> {
    let Some(png) = encode(image, Format::Png, scale) else {
        eprintln!("kadro: failed to encode clipboard image");
        return None;
    };
    let bytes = png.clone();
    // NSPasteboard безопасен только на главном потоке.
    DispatchQueue::main().exec_async(move || copy_to_clipboard(&bytes));
    Some(png)
}

/// Сохраняет снимок в папку по настройкам формата, уменьшения и шаблона имени.
pub fn save(directory: &Path, image: &CGImage, scale: f64, shot: &Shot, png: Option<&[u8]>) -> std::io::Result<PathBuf> {
    let settings = Settings::load();
    let downscaled = (settings.downscale_retina && scale > 1.0).then(|| downscale(image, scale)).flatten();
    let format = match settings.format {
        Format::Auto if looks_like_photo(image) => Format::Jpeg,
        Format::Auto => Format::Png,
        format => format,
    };
    let bytes = match (&downscaled, png) {
        // Тот же снимок в том же формате уже закодирован для буфера обмена.
        (None, Some(png)) if format == Format::Png => Some(png.to_vec()),
        (Some(small), _) => encode(small, format, 1.0),
        (None, _) => encode(image, format, scale),
    };
    let bytes = bytes.ok_or_else(|| std::io::Error::other("encoding failed"))?;
    std::fs::create_dir_all(directory)?;
    let (date, time) = (now("yyyy-MM-dd"), now("HH.mm.ss"));
    let name = file_name(&settings.file_name_template, &date, &time, shot);
    let path = unique_path(directory, &name, format.extension());
    std::fs::write(&path, bytes)?;
    mark_as_screenshot(&path, shot);
    Ok(path)
}

pub fn retain(image: &CGImage) -> CFRetained<CGImage> {
    unsafe { CFRetained::retain(std::ptr::NonNull::from(image)) }
}

/// Кодирует снимок в байты выбранного формата.
fn encode(image: &CGImage, format: Format, scale: f64) -> Option<Vec<u8>> {
    let data = CFMutableData::new(None, 0)?;
    let destination = unsafe {
        CGImageDestination::with_data(&data, &CFString::from_static_str(format.uti()), 1, None)
    }?;
    // DPI 144 у Retina-снимка - чтобы Preview и браузеры показывали его в размере точек, а не пикселей.
    let dpi = CFNumber::new_f64(72.0 * scale);
    let quality = CFNumber::new_f64(0.9);
    let keys = unsafe {
        [kCGImagePropertyDPIWidth, kCGImagePropertyDPIHeight, kCGImageDestinationLossyCompressionQuality]
    };
    let properties = CFDictionary::<CFString, CFNumber>::from_slices(&keys, &[&dpi, &dpi, &quality]);
    unsafe { destination.add_image(image, Some(properties.as_ref())) };
    unsafe { destination.finalize() }.then(|| data.to_vec())
}

/// Уменьшает снимок до размера в точках. Возвращает None, если контекст не создался.
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

/// Имя файла по шаблону с подстановками {date}, {time}, {mode} и {app}.
fn file_name(template: &str, date: &str, time: &str, shot: &Shot) -> String {
    let render = |template: &str| {
        template
            .replace("{date}", date)
            .replace("{time}", time)
            .replace("{mode}", shot.kind.title())
            .replace("{app}", &shot.app)
            // "/" делит путь, а ":" Finder показывает как "/".
            .replace(['/', ':'], "-")
            .trim()
            .to_string()
    };
    let name = render(template);
    // Имя из одних пробелов или начинающееся с точки Finder спрячет или не сохранит.
    if name.is_empty() || name.starts_with('.') { render(DEFAULT_FILE_NAME_TEMPLATE) } else { name }
}

/// Подбирает свободное имя файла, добавляя номер при совпадении.
fn unique_path(directory: &Path, name: &str, extension: &str) -> PathBuf {
    let mut path = directory.join(format!("{name}.{extension}"));
    let mut n = 2;
    while path.exists() {
        path = directory.join(format!("{name} ({n}).{extension}"));
        n += 1;
    }
    path
}

/// Помечает файл как снимок экрана, как это делает системный screencapture,
/// чтобы Finder и Spotlight находили его среди снимков. Ошибки только логируются.
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

/// Похож ли снимок на фото или градиент, которым лучше подходит JPEG.
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
    // Без сглаживания: смешивание пикселей при уменьшении даёт лишние цвета даже у текста.
    CGContext::set_interpolation_quality(Some(&context), CGInterpolationQuality::None);
    CGContext::draw_image(Some(&context), CGRect::new(CGPoint::ZERO, CGSize::new(w as f64, h as f64)), Some(image));
    drop(context);
    is_photographic(&pixels)
}

/// У интерфейса и текста немного разных цветов, у фото и шума почти каждый пиксель свой.
fn is_photographic(pixels: &[u32]) -> bool {
    let unique: HashSet<_> = pixels.iter().map(|p| p & 0x00ff_ffff).collect();
    unique.len() * 4 > pixels.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::capture::Kind;

    #[test]
    fn unique_path_skips_taken_names() {
        let dir = std::env::temp_dir().join(format!("kadro-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "Kadro t", "png");
        assert_eq!(first, dir.join("Kadro t.png"));
        std::fs::write(&first, b"").unwrap();
        assert_eq!(unique_path(&dir, "Kadro t", "png"), dir.join("Kadro t (2).png"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn file_name_fills_tokens_and_falls_back_to_default() {
        let shot = Shot { kind: Kind::Window, global_rect: CGRect::default(), app: "Notes/Work".into() };
        assert_eq!(file_name("{app} {mode} {date}", "2026-11-03", "14.12.00", &shot), "Notes-Work Window 2026-11-03");
        assert_eq!(file_name("Kadro {date} at {time}", "2026-11-03", "14.12.00", &shot), "Kadro 2026-11-03 at 14.12.00");
        // пустое приложение даёт пустое имя
        let empty = Shot { app: String::new(), ..shot };
        assert_eq!(file_name("{app}", "d", "t", &empty), "Kadro d at t");
        assert_eq!(file_name(".hidden", "d", "t", &empty), "Kadro d at t");
    }

    #[test]
    fn flat_interface_is_png_and_noise_is_photo() {
        let interface: Vec<u32> = (0..25_600).map(|i| [0xffffff, 0x1e1e1e, 0x3478f6, 0x888888][i % 4]).collect();
        assert!(!is_photographic(&interface));
        // плавный горизонтальный градиент - один цвет на столбец
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
