//! Распознавание текста и QR- и штрихкодов на снимке средствами Apple Vision, без сети.
//!
//! Распознавание идёт вне главного потока, а результат показывается на главном: ссылку из кода
//! предлагается открыть, остальное копируется в буфер обмена.

use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertSecondButtonReturn, NSApplication, NSBeep, NSPasteboard,
    NSPasteboardTypeString, NSScreen, NSWorkspace,
};
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSArray, NSDictionary, NSLocale, NSString, NSURL, ns_string};
use objc2_vision::{
    VNDetectBarcodesRequest, VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel,
};

use crate::preview;
use crate::settings::Settings;

/// Что нашлось на снимке.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    /// Строки текста сверху вниз.
    pub lines: Vec<String>,
    /// Содержимое QR- и штрихкодов без повторов.
    pub codes: Vec<String>,
}

/// Распознаёт текст и коды. `language` - код языка Vision, None - языки системы и английский.
pub fn recognize(image: &CGImage, language: Option<&str>) -> Found {
    let text = VNRecognizeTextRequest::new();
    text.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    text.setUsesLanguageCorrection(true);
    let languages = match language {
        Some(language) => vec![language.to_string()],
        None => system_languages(&text),
    };
    let languages: Vec<_> = languages.iter().map(|l| NSString::from_str(l)).collect();
    text.setRecognitionLanguages(&NSArray::from_retained_slice(&languages));
    let barcodes = unsafe { VNDetectBarcodesRequest::new() };
    let handler = unsafe {
        VNImageRequestHandler::initWithCGImage_options(VNImageRequestHandler::alloc(), image, &NSDictionary::new())
    };
    let requests: [&VNRequest; 2] = [&text, &barcodes];
    if let Err(err) = handler.performRequests_error(&NSArray::from_slice(&requests)) {
        eprintln!("kadro: text recognition failed: {}", err.localizedDescription());
        return Found::default();
    }
    let lines = text
        .results()
        .map(|results| {
            results
                .iter()
                .filter_map(|observation| observation.topCandidates(1).firstObject())
                .map(|candidate| candidate.string().to_string())
                .collect()
        })
        .unwrap_or_default();
    let mut codes: Vec<String> = Vec::new();
    for observation in unsafe { barcodes.results() }.into_iter().flatten() {
        if let Some(payload) = unsafe { observation.payloadStringValue() }.map(|p| p.to_string())
            && !codes.contains(&payload)
        {
            codes.push(payload);
        }
    }
    Found { lines, codes }
}

/// Языки, которые Vision умеет распознавать.
pub fn languages() -> Vec<String> {
    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    unsafe { request.supportedRecognitionLanguagesAndReturnError() }
        .map(|list| list.iter().map(|l| l.to_string()).collect())
        .unwrap_or_default()
}

/// Предпочитаемые языки системы, которые Vision поддерживает, и английский в конце.
fn system_languages(request: &VNRecognizeTextRequest) -> Vec<String> {
    let supported: Vec<String> = unsafe { request.supportedRecognitionLanguagesAndReturnError() }
        .map(|list| list.iter().map(|l| l.to_string()).collect())
        .unwrap_or_default();
    let mut chosen: Vec<String> = Vec::new();
    for preferred in NSLocale::preferredLanguages().iter().map(|l| l.to_string()) {
        // Система отдаёт «ru-RU» или «ru», у Vision встречаются оба вида.
        let base = preferred.split('-').next().unwrap_or_default().to_string();
        let found = supported.iter().find(|s| **s == preferred || s.split('-').next() == Some(base.as_str()));
        if let Some(language) = found
            && !chosen.contains(language)
        {
            chosen.push(language.clone());
        }
    }
    if !chosen.iter().any(|l| l.starts_with("en")) {
        chosen.push("en-US".into());
    }
    chosen
}

/// Текст из строк: каждая с новой строки или всё в одну строку. При склейке перенос
/// с дефисом посреди слова соединяет его без пробела.
pub fn join(lines: &[String], single_line: bool) -> String {
    if !single_line {
        return lines.join("\n");
    }
    let mut text = String::new();
    for line in lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        let mended = text.strip_suffix('-').filter(|stem| stem.ends_with(char::is_alphabetic));
        if let Some(stem) = mended.filter(|_| line.starts_with(char::is_lowercase)) {
            text = stem.to_string();
        } else if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(line);
    }
    text
}

/// Ссылка из содержимого кода, если это веб-адрес.
pub fn link(payload: &str) -> Option<&str> {
    let lower = payload.to_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://")).then_some(payload)
}

/// Распознаёт снимок вне главного потока и показывает результат на главном экране.
pub fn recognize_and_show(image: objc2_core_foundation::CFRetained<CGImage>) {
    let settings = Settings::load();
    let language = settings.recognition_language.clone();
    crate::output::in_background(move || {
        let found = recognize(&image, language.as_deref());
        dispatch2::DispatchQueue::main().exec_async(move || {
            if let Some(mtm) = MainThreadMarker::new() {
                show(mtm, &found, settings.single_line);
            }
        });
    });
}

/// Ссылку из кода предлагает открыть, остальные коды или текст копирует.
pub fn show(mtm: MainThreadMarker, found: &Found, single_line: bool) {
    let screen = NSScreen::mainScreen(mtm);
    let popup = |text: &str| {
        if let Some(screen) = &screen {
            preview::popup_on(mtm, text, screen);
        }
    };
    if let Some(url) = found.codes.iter().find_map(|code| link(code)) {
        return offer_link(mtm, url);
    }
    if !found.codes.is_empty() {
        copy(&found.codes.join("\n"));
        return popup(if found.codes.len() == 1 { "Copied code" } else { "Copied codes" });
    }
    if found.lines.is_empty() {
        NSBeep();
        return popup("No text found");
    }
    copy(&join(&found.lines, single_line));
    popup("Copied text");
}

fn offer_link(mtm: MainThreadMarker, url: &str) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(ns_string!("Open the link from the code?"));
    alert.setInformativeText(&NSString::from_str(url));
    alert.addButtonWithTitle(ns_string!("Open"));
    alert.addButtonWithTitle(ns_string!("Copy"));
    alert.addButtonWithTitle(ns_string!("Cancel"));
    NSApplication::sharedApplication(mtm).activate();
    let response = alert.runModal();
    if response == NSAlertFirstButtonReturn {
        if let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) {
            NSWorkspace::sharedWorkspace().openURL(&url);
        }
    } else if response == NSAlertSecondButtonReturn {
        copy(url);
    }
}

fn copy(text: &str) {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: системная константа типа буфера обмена живёт весь процесс.
    pasteboard.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_join_into_one_and_mend_hyphenated_words() {
        let lines = ["Kadro takes screen-".to_string(), "shots and".into(), " ".into(), "re-".into(), "42".into()];
        assert_eq!(join(&lines, false), "Kadro takes screen-\nshots and\n \nre-\n42");
        assert_eq!(join(&lines, true), "Kadro takes screenshots and re- 42");
    }

    fn fixture(bytes: &[u8]) -> objc2_core_foundation::CFRetained<CGImage> {
        crate::output::decode(bytes).expect("fixture decodes").0
    }

    #[test]
    fn english_and_russian_text_is_recognized_offline() {
        let english = recognize(&fixture(include_bytes!("../tests/fixtures/english.png")), Some("en-US"));
        assert_eq!(english.lines, ["The quick brown fox jumps", "over the lazy dog 2026"]);
        let russian = recognize(&fixture(include_bytes!("../tests/fixtures/russian.png")), Some("ru-RU"));
        assert_eq!(russian.lines, ["Съешь же ещё этих мягких", "французских булок, да выпей чаю"]);
        assert!(languages().iter().any(|l| l.starts_with("ru")));
    }

    #[test]
    fn qr_code_payload_is_found() {
        let found = recognize(&fixture(include_bytes!("../tests/fixtures/qr.png")), None);
        assert_eq!(found.codes, ["https://github.com/binido/Kadro"]);
        assert_eq!(link(&found.codes[0]), Some("https://github.com/binido/Kadro"));
    }

    #[test]
    fn only_web_addresses_are_links() {
        assert_eq!(link("HTTPS://kadro.app"), Some("HTTPS://kadro.app"));
        assert_eq!(link("http://x.y/z"), Some("http://x.y/z"));
        assert_eq!(link("WIFI:S:home;T:WPA;P:secret;;"), None);
        assert_eq!(link("javascript:alert(1)"), None);
    }
}
