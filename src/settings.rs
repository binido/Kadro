use std::path::PathBuf;

use objc2_foundation::{NSString, NSUserDefaults};

#[derive(Clone, Copy)]
pub enum Format {
    Png,
    Jpeg,
    Heic,
}

impl Format {
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

        let save_to_file = flag("saveToFile", true);
        // Хотя бы одно назначение включено всегда, иначе снимок пропадёт.
        let copy_to_clipboard = flag("copyToClipboard", true) || !save_to_file;
        let format = match string("format").as_deref() {
            Some("jpeg") => Format::Jpeg,
            Some("heic") => Format::Heic,
            _ => Format::Png,
        };
        let directory = string("directory").map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Desktop")
        });

        Settings {
            copy_to_clipboard,
            save_to_file,
            format,
            directory,
            downscale_retina: flag("downscaleRetina", false),
        }
    }
}
