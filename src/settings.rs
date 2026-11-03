use std::path::PathBuf;

use objc2_foundation::{NSNumber, NSString, NSUserDefaults};

pub const COPY_TO_CLIPBOARD: &str = "copyToClipboard";
pub const SAVE_TO_FILE: &str = "saveToFile";
pub const FORMAT: &str = "format";
pub const DIRECTORY: &str = "directory";
pub const DOWNSCALE_RETINA: &str = "downscaleRetina";
pub const CURSOR: &str = "cursor";
pub const FILE_NAME_TEMPLATE: &str = "fileNameTemplate";
pub const LAST_AREA: &str = "lastArea";
pub const SHOW_PREVIEW: &str = "showPreview";
pub const PREVIEW_HIDING: &str = "previewHiding";
pub const CONFIRMATION: &str = "confirmation";

pub const DEFAULT_FILE_NAME_TEMPLATE: &str = "Kadro {date} at {time}";

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Format {
    Png,
    Jpeg,
    Heic,
    /// PNG или JPEG в зависимости от содержимого снимка.
    Auto,
}

impl Format {
    pub const ALL: [Format; 4] = [Format::Png, Format::Jpeg, Format::Heic, Format::Auto];

    pub fn title(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Jpeg => "JPEG",
            Format::Heic => "HEIC",
            Format::Auto => "Auto",
        }
    }

    /// Значение формата в NSUserDefaults.
    pub fn key(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpeg",
            Format::Heic => "heic",
            Format::Auto => "auto",
        }
    }

    // Auto заменяется на PNG или JPEG до кодирования, поэтому у него нет своего типа и расширения.
    pub fn uti(self) -> &'static str {
        match self {
            Format::Png | Format::Auto => "public.png",
            Format::Jpeg => "public.jpeg",
            Format::Heic => "public.heic",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Png | Format::Auto => "png",
            Format::Jpeg => "jpg",
            Format::Heic => "heic",
        }
    }
}

/// Показывать ли курсор на снимке.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Cursor {
    /// Только на снимках всего экрана.
    Auto,
    Show,
    Hide,
}

impl Cursor {
    pub const ALL: [Cursor; 3] = [Cursor::Auto, Cursor::Show, Cursor::Hide];

    pub fn title(self) -> &'static str {
        match self {
            Cursor::Auto => "Auto",
            Cursor::Show => "Show",
            Cursor::Hide => "Hide",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Cursor::Auto => "auto",
            Cursor::Show => "show",
            Cursor::Hide => "hide",
        }
    }
}

/// Когда прятать превью после снимка.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PreviewHiding {
    /// Через несколько секунд без наведения или по клику в другом месте.
    Auto,
    /// Только кнопкой закрытия.
    Manual,
}

impl PreviewHiding {
    pub const ALL: [PreviewHiding; 2] = [PreviewHiding::Auto, PreviewHiding::Manual];

    pub fn title(self) -> &'static str {
        match self {
            PreviewHiding::Auto => "Automatically",
            PreviewHiding::Manual => "Manually",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            PreviewHiding::Auto => "auto",
            PreviewHiding::Manual => "manual",
        }
    }
}

/// Как подтверждать копирование и сохранение, когда превью выключено.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Confirmation {
    /// Собственная всплывающая плашка Kadro.
    Popup,
    /// Уведомление macOS.
    Notification,
    None,
}

impl Confirmation {
    pub const ALL: [Confirmation; 3] = [Confirmation::Popup, Confirmation::Notification, Confirmation::None];

    pub fn title(self) -> &'static str {
        match self {
            Confirmation::Popup => "Pop-up",
            Confirmation::Notification => "Notification",
            Confirmation::None => "None",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Confirmation::Popup => "popup",
            Confirmation::Notification => "notification",
            Confirmation::None => "none",
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
    pub cursor: Cursor,
    pub file_name_template: String,
    pub show_preview: bool,
    pub preview_hiding: PreviewHiding,
    pub confirmation: Confirmation,
}

impl Settings {
    /// Читает настройки, подставляя значения по умолчанию для отсутствующих ключей.
    pub fn load() -> Self {
        let defaults = NSUserDefaults::standardUserDefaults();
        let flag = |key: &str, default: bool| {
            let key = NSString::from_str(key);
            match defaults.objectForKey(&key) {
                Some(_) => defaults.boolForKey(&key),
                None => default,
            }
        };

        let save_to_file = flag(SAVE_TO_FILE, true);
        let show_preview = flag(SHOW_PREVIEW, true);
        // Хотя бы одно действие после снимка включено всегда, иначе снимок пропадёт.
        let copy_to_clipboard = flag(COPY_TO_CLIPBOARD, true) || (!save_to_file && !show_preview);
        let format = string(FORMAT)
            .and_then(|key| Format::ALL.into_iter().find(|f| f.key() == key))
            .unwrap_or(Format::Png);
        let directory = string(DIRECTORY).map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Desktop")
        });
        let cursor = string(CURSOR)
            .and_then(|key| Cursor::ALL.into_iter().find(|c| c.key() == key))
            .unwrap_or(Cursor::Auto);
        let file_name_template = string(FILE_NAME_TEMPLATE)
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_FILE_NAME_TEMPLATE.into());

        let preview_hiding = string(PREVIEW_HIDING)
            .and_then(|key| PreviewHiding::ALL.into_iter().find(|h| h.key() == key))
            .unwrap_or(PreviewHiding::Auto);
        let confirmation = string(CONFIRMATION)
            .and_then(|key| Confirmation::ALL.into_iter().find(|c| c.key() == key))
            .unwrap_or(Confirmation::Popup);

        Settings {
            copy_to_clipboard,
            save_to_file,
            format,
            directory,
            downscale_retina: flag(DOWNSCALE_RETINA, false),
            cursor,
            file_name_template,
            show_preview,
            preview_hiding,
            confirmation,
        }
    }
}

pub fn string(key: &str) -> Option<String> {
    NSUserDefaults::standardUserDefaults().stringForKey(&NSString::from_str(key)).map(|s| s.to_string())
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
