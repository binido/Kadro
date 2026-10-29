use block2::RcBlock;
use objc2::AnyThread;
use objc2_core_foundation::{CGPoint, CGRect};
use objc2_core_graphics::{CGEvent, CGImage, CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration,
};

use crate::output;

/// Снимает целиком дисплей, на котором стоит курсор.
pub fn capture_screen() {
    if !CGPreflightScreenCaptureAccess() {
        // Показывает системный запрос, ответ вступает в силу только после перезапуска.
        CGRequestScreenCaptureAccess();
        return;
    }
    let cursor = CGEvent::location(CGEvent::new(None).as_deref());

    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        let Some(content) = (unsafe { content.as_ref() }) else {
            return report(error);
        };
        let displays = unsafe { content.displays() };
        // SCDisplay.frame и позиция CGEvent в одних координатах - от левого верхнего угла главного дисплея.
        let Some(display) = displays
            .iter()
            .find(|d| contains(unsafe { d.frame() }, cursor))
            .or_else(|| displays.firstObject())
        else {
            return eprintln!("kadro: no displays to capture");
        };

        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &display,
                &NSArray::new(),
            )
        };
        let scale = unsafe { filter.pointPixelScale() } as f64;
        let size = unsafe { filter.contentRect() }.size;
        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            config.setWidth((size.width * scale).round() as usize);
            config.setHeight((size.height * scale).round() as usize);
            config.setShowsCursor(false);
        }
        shoot(&filter, &config, scale);
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
}

fn shoot(filter: &SCContentFilter, config: &SCStreamConfiguration, scale: f64) {
    let handler = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
        match unsafe { image.as_ref() } {
            Some(image) => output::deliver(image, scale),
            None => report(error),
        }
    });
    unsafe {
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
            filter,
            config,
            Some(&handler),
        )
    };
}

fn contains(rect: CGRect, point: CGPoint) -> bool {
    point.x >= rect.origin.x
        && point.x < rect.origin.x + rect.size.width
        && point.y >= rect.origin.y
        && point.y < rect.origin.y + rect.size.height
}

fn report(error: *mut NSError) {
    match unsafe { error.as_ref() } {
        Some(error) => eprintln!("kadro: capture failed: {}", error.localizedDescription()),
        None => eprintln!("kadro: capture failed"),
    }
}
