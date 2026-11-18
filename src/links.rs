//! `kadro://` links for automation: screenshots, text recognition, pinning, upload,
//! opening files and some settings.
//!
//! Links are off by default. When on, they run right away or after a prompt, as chosen
//! in settings, because any app or web page can open a link. Silent captures and upload always ask.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication};
use objc2_foundation::{NSString, ns_string};

use crate::hotkeys::Action;
use crate::settings::{
    self, AfterCapture, Choice, Cursor, Format, LinkPolicy, PreviewHiding, Settings, WindowBackground,
};
use crate::{editor, prefs};

pub const SCHEME: &str = "kadro";

/// What a link asks for.
#[derive(Clone, PartialEq, Debug)]
pub enum Command {
    Run(Action),
    Open(PathBuf),
    Clipboard,
    Settings,
    /// Settings key and value pairs, already validated.
    Set(Vec<(&'static str, String)>),
}

impl Command {
    /// Whether the command captures or uploads with no further step from the user.
    fn is_silent(&self) -> bool {
        matches!(
            self,
            Command::Run(
                Action::Screen
                    | Action::DelayedScreen
                    | Action::ActiveWindow
                    | Action::RepeatRegion
                    | Action::UploadLast
            )
        )
    }

    /// What the command does, in words for the prompt. Paths and values are already decoded.
    fn describe(&self) -> String {
        match self {
            Command::Run(action) => action.title().to_string(),
            Command::Open(path) => format!("Open {} in the editor", path.display()),
            Command::Clipboard => "Open the image on the clipboard in the editor".into(),
            Command::Settings => "Open Settings".into(),
            Command::Set(pairs) => {
                let changes: Vec<_> = pairs.iter().map(|(key, value)| format!("{key} = {value}")).collect();
                format!("Change settings: {}", changes.join(", "))
            }
        }
    }
}

/// Link paths and the actions they run. The same list is in docs/guide.md.
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

/// Parses a link. The error is a readable explanation of what is wrong with it.
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
        // Otherwise text in an ignored query could make the prompt look harmless.
        if !pairs.is_empty() {
            return Err(format!("{SCHEME}://{path} takes no parameters"));
        }
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

/// Validates one setting and returns its `NSUserDefaults` key and the value to write.
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
        "folder" => {
            let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
            private_folder(Path::new(value), &home)
                .map(|folder| (settings::DIRECTORY, folder.to_string_lossy().into_owned()))
                .ok_or_else(|| "folder must be an existing folder in your home folder that only you can open".into())
        }
        _ => Err(format!("{key} can't be set from a link")),
    }
}

/// The resolved folder if a link may send screenshots there: it exists, lies inside `home`
/// and gives other accounts no access.
fn private_folder(folder: &Path, home: &Path) -> Option<PathBuf> {
    // Resolved so ".." and symlinks can't lead outside the home folder.
    let folder = folder.canonicalize().ok()?;
    let metadata = std::fs::metadata(&folder).ok()?;
    let private = metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0;
    (private && folder.starts_with(home.canonicalize().ok()?)).then_some(folder)
}

/// Decodes `%XX` and `+` in a query string. An invalid sequence is kept as is.
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

/// Runs a link if links are on, and in prompt mode after the user agrees.
pub fn handle(mtm: MainThreadMarker, url: &str) {
    let command = match parse(url) {
        Ok(command) => command,
        Err(message) => return explain(mtm, "Kadro couldn't follow the link", &message),
    };
    let settings = Settings::load();
    if !settings.links_enabled {
        let alert = NSAlert::new(mtm);
        alert.setMessageText(ns_string!("Kadro links are turned off"));
        alert.setInformativeText(&NSString::from_str(&format!(
                "An app or page asked Kadro to open {url}. Turn on kadro:// links in Settings → General → Advanced if you use them."
            )));
        alert.addButtonWithTitle(ns_string!("Open Settings"));
        alert.addButtonWithTitle(ns_string!("Ignore"));
        NSApplication::sharedApplication(mtm).activate();
        if alert.runModal() == NSAlertFirstButtonReturn {
            prefs::show(mtm);
        }
        return;
    }
    // A capture or upload with no selection step is asked about even in Allow mode, because any page can open a link.
    if settings.links == LinkPolicy::Ask || command.is_silent() {
        let alert = NSAlert::new(mtm);
        alert.setMessageText(ns_string!("Follow this Kadro link?"));
        alert.setInformativeText(&NSString::from_str(&format!("{}.\n\nLink: {url}", command.describe())));
        alert.addButtonWithTitle(ns_string!("Allow"));
        alert.addButtonWithTitle(ns_string!("Cancel"));
        NSApplication::sharedApplication(mtm).activate();
        if alert.runModal() != NSAlertFirstButtonReturn {
            return;
        }
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
            parse("kadro://set?format=jpeg&copy=off&after=editor"),
            Ok(Command::Set(vec![
                ("format", "jpeg".into()),
                (settings::COPY_TO_CLIPBOARD, "false".into()),
                ("afterCapture", "editor".into()),
            ]))
        );
        assert!(parse("kadro://set?format=jpeg&folder=%2Ftmp").is_err());
        assert!(parse("kadro://set?folder=%2FUsers%2FShared").is_err());
        assert!(parse("kadro://set?format=bmp").unwrap_err().contains("png, jpeg, heic, auto"));
        assert!(parse("kadro://set?copy=maybe").is_err());
        assert!(parse("kadro://set?secret=1").unwrap_err().contains("can't be set"));
        assert!(parse("kadro://set").is_err());
    }

    #[test]
    fn folder_from_a_link_must_be_private_and_inside_home() {
        let root = std::env::temp_dir().join(format!("kadro-links-{}", std::process::id()));
        let home = root.join("home");
        let private = home.join("Shots");
        let open = home.join("Public");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::create_dir_all(&open).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(private_folder(&private, &home), Some(private.canonicalize().unwrap()));
        // readable by other accounts
        assert_eq!(private_folder(&open, &home), None);
        // outside home, even through ".."
        assert_eq!(private_folder(&private.join("../../.."), &home), None);
        assert_eq!(private_folder(&home.join("missing"), &home), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn routes_refuse_parameters_and_silent_captures_are_marked() {
        assert!(parse("kadro://upload?note=nothing%20leaves%20this%20Mac").unwrap_err().contains("no parameters"));
        assert!(parse("kadro://capture/screen").unwrap().is_silent());
        assert!(parse("kadro://upload").unwrap().is_silent());
        assert!(!parse("kadro://capture/region").unwrap().is_silent());
        assert!(!parse("kadro://settings").unwrap().is_silent());
    }

    #[test]
    fn prompt_text_shows_decoded_values() {
        assert_eq!(parse("kadro://capture/screen").unwrap().describe(), "Capture Full Screen");
        assert_eq!(
            parse("kadro://open?path=%2Ftmp%2FShot%201.png").unwrap().describe(),
            "Open /tmp/Shot 1.png in the editor"
        );
        assert_eq!(parse("kadro://set?copy=off").unwrap().describe(), "Change settings: copyToClipboard = false");
    }

    #[test]
    fn decoding_handles_escapes_plus_and_broken_escapes() {
        assert_eq!(decode("a%20b+c%2F%D0%AF"), "a b c/Я");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
    }
}
