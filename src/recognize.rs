//! Text, QR code and barcode recognition on a screenshot with Apple Vision, offline.
//!
//! Recognition runs off the main thread and the result is shown on the main thread: a link from a code
//! is offered to open, everything else is copied to the clipboard.

use std::sync::atomic::{AtomicU64, Ordering};

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

/// What was found on the screenshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    /// Text lines from top to bottom.
    pub lines: Vec<String>,
    /// Contents of QR codes and barcodes, without duplicates.
    pub codes: Vec<String>,
}

/// Recognizes text and codes. `language` is a Vision language code, None means system languages and English.
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

/// Command line flag: Kadro recognizes the listed files, prints the result and exits.
const CHILD_FLAG: &str = "--recognize";
/// Separator between lines and codes in the separate process output.
const SEPARATOR: char = '\u{1e}';

/// Recognizes files in a separate Kadro process. After running, Vision keeps tens of megabytes
/// of models in the process, and a separate process returns them to the system when it exits.
/// Called only off the main thread. A file that could not be read gets an empty result.
pub fn recognize_files(paths: &[std::path::PathBuf], language: Option<&str>) -> Vec<Found> {
    let output = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe).arg(CHILD_FLAG).arg(language.unwrap_or("-")).args(paths).output()
    });
    let text = match output {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(err) => {
            eprintln!("kadro: failed to start recognition: {err}");
            String::new()
        }
    };
    let mut found: Vec<Found> = text.lines().map(parse_found).collect();
    found.resize(paths.len(), Found::default());
    found
}

/// If Kadro was launched for recognition, recognizes the files from the arguments, prints
/// one line per file and returns true. Otherwise does nothing.
pub fn run_child() -> bool {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some(CHILD_FLAG) {
        return false;
    }
    let language = args.next().filter(|l| l != "-");
    for path in args {
        let found = std::fs::read(&path)
            .ok()
            .and_then(|bytes| crate::output::decode(&bytes))
            .map(|(image, _)| recognize(&image, language.as_deref()))
            .unwrap_or_default();
        println!("{}", format_found(&found));
    }
    true
}

/// Result for one image on one line: text lines, a tab, code contents.
fn format_found(found: &Found) -> String {
    let clean = |text: &String| text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>();
    let join = |items: &[String]| items.iter().map(clean).collect::<Vec<_>>().join(&SEPARATOR.to_string());
    format!("{}\t{}", join(&found.lines), join(&found.codes))
}

fn parse_found(line: &str) -> Found {
    let (lines, codes) = line.split_once('\t').unwrap_or((line, ""));
    let split = |text: &str| text.split(SEPARATOR).filter(|s| !s.is_empty()).map(String::from).collect();
    Found { lines: split(lines), codes: split(codes) }
}

/// Languages Vision can recognize.
pub fn languages() -> Vec<String> {
    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    unsafe { request.supportedRecognitionLanguagesAndReturnError() }
        .map(|list| list.iter().map(|l| l.to_string()).collect())
        .unwrap_or_default()
}

/// Preferred system languages that Vision supports, with English at the end.
fn system_languages(request: &VNRecognizeTextRequest) -> Vec<String> {
    let supported: Vec<String> = unsafe { request.supportedRecognitionLanguagesAndReturnError() }
        .map(|list| list.iter().map(|l| l.to_string()).collect())
        .unwrap_or_default();
    let mut chosen: Vec<String> = Vec::new();
    for preferred in NSLocale::preferredLanguages().iter().map(|l| l.to_string()) {
        // The system reports "ru-RU" or "ru", Vision has both forms.
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

/// Text from lines: each on its own line or all on one line. When joining, a hyphenated
/// break in the middle of a word joins it without a space.
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

/// Link from code contents, if it is a web address.
pub fn link(payload: &str) -> Option<&str> {
    let lower = payload.to_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://")).then_some(payload)
}

/// Recognizes the screenshot off the main thread and shows the result on the main thread.
pub fn recognize_and_show(image: objc2_core_foundation::CFRetained<CGImage>) {
    let settings = Settings::load();
    let language = settings.recognition_language.clone();
    // A name per run, otherwise two recognitions at once would read each other's image.
    static RUN: AtomicU64 = AtomicU64::new(0);
    let run = RUN.fetch_add(1, Ordering::Relaxed);
    crate::output::in_background(move || {
        let file = std::env::temp_dir().join("Kadro").join(format!("recognize-{run}.png"));
        let written = std::fs::create_dir_all(file.parent().unwrap_or(&file))
            .and_then(|()| crate::output::write_to(&file, &image, 1.0));
        let found = match written {
            Ok(()) => recognize_files(std::slice::from_ref(&file), language.as_deref()).pop().unwrap_or_default(),
            Err(err) => {
                eprintln!("kadro: failed to prepare an image for recognition: {err}");
                Found::default()
            }
        };
        let _ = std::fs::remove_file(&file);
        dispatch2::DispatchQueue::main().exec_async(move || {
            if let Some(mtm) = MainThreadMarker::new() {
                show(mtm, &found, settings.single_line);
            }
        });
    });
}

/// Offers to open a link from a code, copies other codes or text.
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
    // SAFETY: the system pasteboard type constant lives for the whole process.
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
    fn child_output_round_trips_text_and_codes() {
        let found =
            Found { lines: vec!["Line one".into(), "tab\there".into()], codes: vec!["https://kadro.app".into()] };
        let line = format_found(&found);
        assert!(!line.contains('\n'));
        assert_eq!(parse_found(&line), Found { lines: vec!["Line one".into(), "tab here".into()], codes: found.codes });
        assert_eq!(parse_found("\t"), Found::default());
    }

    #[test]
    fn only_web_addresses_are_links() {
        assert_eq!(link("HTTPS://kadro.app"), Some("HTTPS://kadro.app"));
        assert_eq!(link("http://x.y/z"), Some("http://x.y/z"));
        assert_eq!(link("WIFI:S:home;T:WPA;P:secret;;"), None);
        assert_eq!(link("javascript:alert(1)"), None);
    }
}
