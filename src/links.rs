//! Ссылки `kadro://` для автоматизации: снимки, распознавание, закрепление, загрузка,
//! открытие файлов и часть настроек.
//!
//! По умолчанию ссылки выключены. Включённые выполняются сразу или после вопроса, как выбрано
//! в настройках, потому что ссылку может открыть любое приложение или страница.

use std::path::PathBuf;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication};
use objc2_foundation::{NSString, ns_string};

use crate::hotkeys::Action;
use crate::settings::{
    self, AfterCapture, Choice, Cursor, Format, LinkPolicy, PreviewHiding, Settings, WindowBackground,
};
use crate::{editor, prefs};

pub const SCHEME: &str = "kadro";

/// Что просит ссылка.
#[derive(Clone, PartialEq, Debug)]
pub enum Command {
    Run(Action),
    Open(PathBuf),
    Clipboard,
    Settings,
    /// Пары ключ и значение настроек, уже проверенные.
    Set(Vec<(&'static str, String)>),
}

/// Пути ссылок и действия, которые они запускают. Тот же список - в README.
const ROUTES: [(&str, Action); 12] = [
    ("capture/region", Action::Region),
    ("capture/repeat", Action::RepeatRegion),
    ("capture/screen", Action::Screen),
    ("capture/delayed", Action::DelayedScreen),
    ("capture/window", Action::Window),
    ("capture/active-window", Action::ActiveWindow),
    ("capture/scrolling", Action::ScrollCapture),
    ("recognize", Action::RecognizeText),
    ("pin", Action::PinLast),
    ("edit", Action::EditLast),
    ("upload", Action::UploadLast),
    ("library", Action::OpenLibrary),
];

/// Разбирает ссылку. Ошибка - понятное объяснение, что в ней не так.
pub fn parse(url: &str) -> Result<Command, String> {
    let rest = url
        .strip_prefix(&format!("{SCHEME}://"))
        .or_else(|| url.strip_prefix(&format!("{SCHEME}:")))
        .ok_or_else(|| format!("not a {SCHEME}:// link"))?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let path = path.trim_matches('/');
    let pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(key), decode(value))
        })
        .collect();
    if let Some((_, action)) = ROUTES.iter().find(|(route, _)| *route == path) {
        return Ok(Command::Run(*action));
    }
    match path {
        "open" => {
            let file = pairs.iter().find(|(k, _)| k == "path").map(|(_, v)| v.clone()).unwrap_or_default();
            if file.starts_with('/') {
                Ok(Command::Open(PathBuf::from(file)))
            } else {
                Err("open needs path=/absolute/path/to/image".into())
            }
        }
        "clipboard" => Ok(Command::Clipboard),
        "settings" => Ok(Command::Settings),
        "set" if pairs.is_empty() => Err("set needs at least one setting, for example set?format=png".into()),
        "set" => pairs.iter().map(|(k, v)| setting(k, v)).collect::<Result<_, _>>().map(Command::Set),
        _ => Err(format!("unknown link {SCHEME}://{path}")),
    }
}

/// Проверяет одну настройку и возвращает её ключ в `NSUserDefaults` и значение для записи.
fn setting(key: &str, value: &str) -> Result<(&'static str, String), String> {
    fn choice<T: Choice>(value: &str) -> Result<(&'static str, String), String> {
        let known: Vec<_> = T::ALL.iter().map(|c| c.key()).collect();
        T::from_key(value)
            .map(|c| (T::DEFAULTS_KEY, c.key().to_string()))
            .ok_or_else(|| format!("{value} is not one of {}", known.join(", ")))
    }
    let flag = |defaults_key: &'static str| match value {
        "true" | "on" | "yes" | "1" => Ok((defaults_key, "true".to_string())),
        "false" | "off" | "no" | "0" => Ok((defaults_key, "false".to_string())),
        _ => Err(format!("{key} is on or off, not {value}")),
    };
    match key {
        "format" => choice::<Format>(value),
        "cursor" => choice::<Cursor>(value),
        "after" => choice::<AfterCapture>(value),
        "hide-preview" => choice::<PreviewHiding>(value),
        "window" => choice::<WindowBackground>(value),
        "copy" => flag(settings::COPY_TO_CLIPBOARD),
        "save" => flag(settings::SAVE_TO_FILE),
        "folder" if value.starts_with('/') => Ok((settings::DIRECTORY, value.to_string())),
        "folder" => Err("folder needs an absolute path".into()),
        _ => Err(format!("{key} can't be set from a link")),
    }
}

/// Раскодирует `%XX` и `+` из строки запроса. Неправильная последовательность остаётся как есть.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| text.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (escaped, bytes[i]) {
            (Some(byte), _) => {
                out.push(byte);
                i += 3;
                continue;
            }
            (None, b'+') => out.push(b' '),
            (None, byte) => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Выполняет ссылку, если ссылки включены, а в режиме вопроса - после согласия.
pub fn handle(mtm: MainThreadMarker, url: &str) {
    let command = match parse(url) {
        Ok(command) => command,
        Err(message) => return explain(mtm, "Kadro couldn't follow the link", &message),
    };
    match Settings::load().links {
        LinkPolicy::Off => {
            let alert = NSAlert::new(mtm);
            alert.setMessageText(ns_string!("Kadro links are turned off"));
            alert.setInformativeText(&NSString::from_str(&format!(
                "An app or page asked Kadro to open {url}. Turn on links in Settings → General if you use them."
            )));
            alert.addButtonWithTitle(ns_string!("Open Settings"));
            alert.addButtonWithTitle(ns_string!("Ignore"));
            NSApplication::sharedApplication(mtm).activate();
            if alert.runModal() == NSAlertFirstButtonReturn {
                prefs::show(mtm);
            }
            return;
        }
        LinkPolicy::Ask => {
            let alert = NSAlert::new(mtm);
            alert.setMessageText(ns_string!("Follow this Kadro link?"));
            alert.setInformativeText(&NSString::from_str(url));
            alert.addButtonWithTitle(ns_string!("Allow"));
            alert.addButtonWithTitle(ns_string!("Cancel"));
            NSApplication::sharedApplication(mtm).activate();
            if alert.runModal() != NSAlertFirstButtonReturn {
                return;
            }
        }
        LinkPolicy::Allow => {}
    }
    run(mtm, command);
}

fn run(mtm: MainThreadMarker, command: Command) {
    match command {
        Command::Run(action) => action.run(mtm),
        Command::Open(path) => editor::open_file(mtm, &path),
        Command::Clipboard => editor::open_clipboard(mtm),
        Command::Settings => prefs::show(mtm),
        Command::Set(pairs) => {
            for (key, value) in pairs {
                match value.as_str() {
                    "true" => settings::set_flag(key, true),
                    "false" => settings::set_flag(key, false),
                    _ => settings::set_string(key, &value),
                }
            }
        }
    }
}

fn explain(mtm: MainThreadMarker, title: &str, message: &str) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(message));
    NSApplication::sharedApplication(mtm).activate();
    alert.runModal();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_route_starts_its_action() {
        for (route, action) in ROUTES {
            assert_eq!(parse(&format!("kadro://{route}")), Ok(Command::Run(action)), "{route}");
        }
        assert_eq!(parse("kadro://capture/region/"), Ok(Command::Run(Action::Region)));
        assert_eq!(parse("kadro:clipboard"), Ok(Command::Clipboard));
        assert_eq!(parse("kadro://settings"), Ok(Command::Settings));
        assert!(parse("kadro://capture/everything").unwrap_err().contains("unknown link"));
        assert!(parse("https://kadro.app").is_err());
    }

    #[test]
    fn open_takes_an_absolute_percent_encoded_path() {
        assert_eq!(
            parse("kadro://open?path=%2FUsers%2Fme%2FDesktop%2FShot%201.png"),
            Ok(Command::Open(PathBuf::from("/Users/me/Desktop/Shot 1.png")))
        );
        assert_eq!(parse("kadro://open?path=/tmp/a+b.png"), Ok(Command::Open(PathBuf::from("/tmp/a b.png"))));
        assert!(parse("kadro://open?path=relative.png").is_err());
        assert!(parse("kadro://open").is_err());
    }

    #[test]
    fn set_checks_every_value_before_changing_anything() {
        assert_eq!(
            parse("kadro://set?format=jpeg&copy=off&after=editor&folder=%2Ftmp%2Fshots"),
            Ok(Command::Set(vec![
                ("format", "jpeg".into()),
                (settings::COPY_TO_CLIPBOARD, "false".into()),
                ("afterCapture", "editor".into()),
                (settings::DIRECTORY, "/tmp/shots".into()),
            ]))
        );
        assert!(parse("kadro://set?format=bmp").unwrap_err().contains("png, jpeg, heic, auto"));
        assert!(parse("kadro://set?copy=maybe").is_err());
        assert!(parse("kadro://set?secret=1").unwrap_err().contains("can't be set"));
        assert!(parse("kadro://set").is_err());
    }

    #[test]
    fn decoding_handles_escapes_plus_and_broken_escapes() {
        assert_eq!(decode("a%20b+c%2F%D0%AF"), "a b c/Я");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
    }
}
