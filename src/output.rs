//! Что происходит со снимком после захвата: буфер обмена, файл и метаданные Finder.

use std::collections::HashSet;
use std::ffi::{CString, c_char, c_int, c_void};
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

/// Куда идёт следующий снимок области вместо обычного вывода.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Route {
    Normal,
    /// На холст редактора, который его ждёт.
    Editor,
    /// Только в распознавание текста.
    Text,
    /// Выделенная область прокручивается и склеивается, а не снимается сразу.
    Scroll,
}

/// Маршрут следующего снимка. Ставится на главном потоке, читается в потоке захвата.
static ROUTE: AtomicU8 = AtomicU8::new(0);

/// Направляет следующий снимок области мимо буфера обмена, файла и превью.
pub fn route_next(route: Route) {
    ROUTE.store(route as u8, Ordering::SeqCst);
}

/// Выделение отменили, следующий снимок идёт обычным путём.
pub fn cancel_route() {
    route_next(Route::Normal);
}

/// Забирает маршрут прокрутки, если выделение было для неё. Другие маршруты остаются на месте.
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

/// Отдаёт снимок: подкладывает фон за окном, если он выбран, затем копирует, сохраняет и показывает.
pub fn deliver(image: &CGImage, scale: f64, shot: &Shot) {
    // Снимок для редактора или распознавания не копируется, не сохраняется и не становится последним.
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
        // Обои экрана узнаются на главном потоке, а рисование и вывод снова уходят в фон.
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
    finish(image, scale, shot);
}

/// Копирует и сохраняет готовый снимок, затем показывает превью или подтверждение.
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

/// Выполняет работу с файлами и кодированием вне главного потока, чтобы интерфейс не замирал.
pub fn in_background(work: impl FnOnce() + Send + 'static) {
    DispatchQueue::global_queue(GlobalQueueIdentifier::QualityOfService(DispatchQoS::UserInitiated)).exec_async(work);
}

/// Кодирует и кладёт снимок в буфер обмена в фоне.
pub fn copy_in_background(image: CFRetained<CGImage>, scale: f64) {
    in_background(move || {
        copy(&image, scale);
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
/// Без `shot` - картинка открыта из файла или буфера, а не снята, и метки снимка экрана ей не ставятся.
pub fn save(
    directory: &Path,
    image: &CGImage,
    scale: f64,
    shot: Option<&Shot>,
    png: Option<&[u8]>,
) -> std::io::Result<PathBuf> {
    let (bytes, name, format) = encoded(image, scale, shot, png)?;
    std::fs::create_dir_all(directory)?;
    let path = unique_path(directory, &name, format.extension());
    std::fs::write(&path, bytes)?;
    if let Some(shot) = shot {
        mark_as_screenshot(&path, shot);
    }
    library::add(&path);
    Ok(path)
}

/// Снимок в формате из настроек и имя файла без расширения по шаблону.
/// `png` - уже закодированный PNG, если он есть.
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
        // Тот же снимок в том же формате уже закодирован для буфера обмена.
        (None, Some(png)) if format == Format::Png => Some(png.to_vec()),
        (Some(small), _) => encode(small, format, 1.0),
        (None, _) => encode(image, format, scale),
    };
    let bytes = bytes.ok_or_else(|| std::io::Error::other(format!("{} encoding failed", format.extension())))?;
    let (date, time) = (now("yyyy-MM-dd"), now("HH.mm.ss"));
    let (mode, app) = shot.map_or(("Image", ""), |shot| (shot.kind.title(), shot.app.as_str()));
    Ok((bytes, file_name(&settings.file_name_template, &date, &time, mode, app), format))
}

/// Записывает картинку в выбранный путь. Формат берётся из расширения, незнакомое расширение - ошибка.
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

/// Разбирает картинку и её масштаб из DPI. Без DPI считается, что пиксель равен точке.
pub fn decode(bytes: &[u8]) -> Option<(CFRetained<CGImage>, f64)> {
    let source = unsafe { CGImageSource::with_data(&CFData::from_bytes(bytes), None) }?;
    let image = unsafe { source.image_at_index(0, None) }?;
    let dpi = unsafe { source.properties_at_index(0, None) }.and_then(|properties| {
        // SAFETY: свойства изображения - словарь со строковыми ключами, CFDictionary и NSDictionary взаимозаменяемы.
        let properties: &NSDictionary<NSString, AnyObject> = unsafe { bridge::toll_free(&*properties) };
        // SAFETY: ключ - константа ImageIO типа CFString, а CFString и NSString взаимозаменяемы.
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

/// Кодирует снимок в байты выбранного формата.
fn encode(image: &CGImage, format: Format, scale: f64) -> Option<Vec<u8>> {
    let data = CFMutableData::new(None, 0)?;
    let destination =
        unsafe { CGImageDestination::with_data(&data, &CFString::from_static_str(format.uti()), 1, None) }?;
    // DPI 144 у Retina-снимка - чтобы Preview и браузеры показывали его в размере точек, а не пикселей.
    let dpi = CFNumber::new_f64(72.0 * scale);
    let quality = CFNumber::new_f64(0.9);
    let keys =
        unsafe { [kCGImagePropertyDPIWidth, kCGImagePropertyDPIHeight, kCGImageDestinationLossyCompressionQuality] };
    let properties = CFDictionary::<CFString, CFNumber>::from_slices(&keys, &[&dpi, &dpi, &quality]);
    unsafe { destination.add_image(image, Some(properties.as_ref())) };
    unsafe { destination.finalize() }.then(|| data.to_vec())
}

/// Анимированный GIF из кадров, которые сменяют друг друга по кругу через `seconds`.
pub fn encode_gif(frames: &[&CGImage], seconds: f64) -> Option<Vec<u8>> {
    let data = CFMutableData::new(None, 0)?;
    let gif = CFString::from_static_str("com.compuserve.gif");
    let destination = unsafe { CGImageDestination::with_data(&data, &gif, frames.len(), None) }?;
    // SAFETY: ключи - константы ImageIO, они живут весь процесс.
    let (dictionary, loop_count, delay) =
        unsafe { (kCGImagePropertyGIFDictionary, kCGImagePropertyGIFLoopCount, kCGImagePropertyGIFDelayTime) };
    let gif_properties = |key: &CFString, value: &CFNumber| {
        let inner = CFDictionary::<CFString, CFNumber>::from_slices(&[key], &[value]);
        CFDictionary::<CFString, CFDictionary<CFString, CFNumber>>::from_slices(&[dictionary], &[&inner])
    };
    // 0 повторов означает бесконечный круг.
    unsafe { destination.set_properties(Some(gif_properties(loop_count, &CFNumber::new_i32(0)).as_ref())) };
    let frame = gif_properties(delay, &CFNumber::new_f64(seconds));
    for image in frames {
        unsafe { destination.add_image(image, Some(frame.as_ref())) };
    }
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
fn file_name(template: &str, date: &str, time: &str, mode: &str, app: &str) -> String {
    let render = |template: &str| {
        template
            .replace("{date}", date)
            .replace("{time}", time)
            .replace("{mode}", mode)
            .replace("{app}", app)
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
        assert_eq!(
            file_name("{app} {mode} {date}", "2026-11-03", "14.12.00", "Window", "Notes/Work"),
            "Notes-Work Window 2026-11-03"
        );
        assert_eq!(
            file_name("Kadro {date} at {time}", "2026-11-03", "14.12.00", "Window", ""),
            "Kadro 2026-11-03 at 14.12.00"
        );
        // пустое приложение даёт пустое имя
        assert_eq!(file_name("{app}", "d", "t", "Window", ""), "Kadro d at t");
        assert_eq!(file_name(".hidden", "d", "t", "Window", ""), "Kadro d at t");
    }

    #[test]
    fn gif_has_both_frames_and_loops_forever() {
        let space = CGColorSpace::new_device_rgb();
        let frame = |gray: f64| {
            // SAFETY: буфер null - Core Graphics выделит его сам.
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
        // ImageIO пишет заголовок GIF87a, но расширения повтора и задержки на месте
        assert_eq!(&gif[..3], b"GIF");
        let source = unsafe { CGImageSource::with_data(&CFData::from_bytes(&gif), None) }.unwrap();
        assert_eq!(unsafe { source.count() }, 2);
        // NETSCAPE2.0 с нулём повторов - бесконечный круг, как его понимают Safari и Preview
        let netscape = gif.windows(11).position(|w| w == b"NETSCAPE2.0").expect("loop extension");
        assert_eq!(&gif[netscape + 13..netscape + 15], &[0, 0]);
        // у каждого кадра задержка 100 сотых секунды
        let delays = gif.windows(6).filter(|w| w[..3] == [0x21, 0xf9, 0x04] && w[4..] == [100, 0]).count();
        assert_eq!(delays, 2);
    }

    #[test]
    fn write_to_picks_the_format_from_the_extension() {
        let space = CGColorSpace::new_device_rgb();
        // SAFETY: буфер null - Core Graphics выделит его сам.
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
