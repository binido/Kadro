use std::path::{Path, PathBuf};

use dispatch2::DispatchQueue;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG};
use objc2_core_foundation::{CFDictionary, CFMutableData, CFNumber, CFRetained, CFString, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGContext, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, CGInterpolationQuality,
};
use objc2_foundation::{NSData, NSDate, NSDateFormatter, NSString};
use objc2_image_io::{
    kCGImageDestinationLossyCompressionQuality, kCGImagePropertyDPIHeight, kCGImagePropertyDPIWidth,
    CGImageDestination,
};

use crate::settings::{Format, Settings};

/// Отдаёт снимок в буфер обмена и в файл согласно настройкам.
pub fn deliver(image: &CGImage, scale: f64) {
    let settings = Settings::load();

    if settings.copy_to_clipboard {
        match encode(image, Format::Png, scale) {
            Some(png) => DispatchQueue::main().exec_async(move || copy_to_clipboard(&png)),
            None => eprintln!("kadro: failed to encode clipboard image"),
        }
    }

    if settings.save_to_file {
        let downscaled = (settings.downscale_retina && scale > 1.0)
            .then(|| downscale(image, scale))
            .flatten();
        let (image, scale) = match &downscaled {
            Some(small) => (&**small, 1.0),
            None => (image, scale),
        };
        let format = settings.format;
        let saved = encode(image, format, scale).ok_or_else(|| std::io::Error::other("encoding failed")).and_then(|bytes| {
            std::fs::create_dir_all(&settings.directory)?;
            let path = unique_path(&settings.directory, &timestamp(), format.extension());
            std::fs::write(&path, bytes).map(|_| path)
        });
        if let Err(err) = saved {
            eprintln!("kadro: failed to save screenshot: {err}");
        }
    }
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

fn timestamp() -> String {
    let formatter = NSDateFormatter::new();
    formatter.setDateFormat(Some(&NSString::from_str("yyyy-MM-dd 'at' HH.mm.ss")));
    formatter.stringFromDate(&NSDate::now()).to_string()
}

/// Подбирает свободное имя файла, добавляя номер при совпадении.
fn unique_path(directory: &Path, timestamp: &str, extension: &str) -> PathBuf {
    let mut path = directory.join(format!("Kadro {timestamp}.{extension}"));
    let mut n = 2;
    while path.exists() {
        path = directory.join(format!("Kadro {timestamp} ({n}).{extension}"));
        n += 1;
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_path_skips_taken_names() {
        let dir = std::env::temp_dir().join(format!("kadro-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "t", "png");
        assert_eq!(first, dir.join("Kadro t.png"));
        std::fs::write(&first, b"").unwrap();
        assert_eq!(unique_path(&dir, "t", "png"), dir.join("Kadro t (2).png"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
