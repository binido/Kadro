use std::path::PathBuf;

use objc2_foundation::{NSNumber, NSString, NSUserDefaults};

pub const COPY_TO_CLIPBOARD: &str = "copyToClipboard";
pub const SAVE_TO_FILE: &str = "saveToFile";
pub const FORMAT: &str = "format";
pub const DIRECTORY: &str = "directory";
pub const DOWNSCALE_RETINA: &str = "downscaleRetina";

#[derive(Clone, Copy)]
pub enum Format {
    Png,
    Jpeg,
    Heic,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Png, Format::Jpeg, Format::Heic];

    pub fn title(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Jpeg => "JPEG",
            Format::Heic => "HEIC",
        }
    }

    /// Значение формата в NSUserDefaults.
    pub fn key(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpeg",
            Format::Heic => "heic",
        }
    }

    pub fn uti(self) -> &'static str {
        match self {
            Format::Png => "public.png",
            Format::Jpeg => "public.jpeg",
            Format::Heic => "public.heic",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpg",
            Format::Heic => "heic",
        }
    }
}

/// Пользовательские настройки из NSUserDefaults.
pub struct Settings {
    pub copy_to_clipboard: bool,
    pub save_to_file: bool,
    pub format: Format,
    pub directory: PathBuf,
    pub downscale_retina: bool,
}

impl Settings {
    /// Читает настройки, подставляя значения по умолчанию для отсутствующих ключей.
    pub fn load() -> Self {
        let defaults = NSUserDefaults::standardUserDefaults();
        let string = |key: &str| defaults.stringForKey(&NSString::from_str(key)).map(|s| s.to_string());
        let flag = |key: &str, default: bool| {
            let key = NSString::from_str(key);
            match defaults.objectForKey(&key) {
                Some(_) => defaults.boolForKey(&key),
                None => default,
            }
        };

        let save_to_file = flag(SAVE_TO_FILE, true);
        // Хотя бы одно назначение включено всегда, иначе снимок пропадёт.
        let copy_to_clipboard = flag(COPY_TO_CLIPBOARD, true) || !save_to_file;
        let format = string(FORMAT)
            .and_then(|key| Format::ALL.into_iter().find(|f| f.key() == key))
            .unwrap_or(Format::Png);
        let directory = string(DIRECTORY).map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Desktop")
        });

        Settings {
            copy_to_clipboard,
            save_to_file,
            format,
            directory,
            downscale_retina: flag(DOWNSCALE_RETINA, false),
        }
    }
}

pub fn set_flag(key: &str, value: bool) {
    let key = NSString::from_str(key);
    unsafe { NSUserDefaults::standardUserDefaults().setObject_forKey(Some(&NSNumber::new_bool(value)), &key) };
}

pub fn set_string(key: &str, value: &str) {
    let key = NSString::from_str(key);
    unsafe {
        NSUserDefaults::standardUserDefaults().setObject_forKey(Some(&NSString::from_str(value)), &key)
    };
}
