//! Пользовательские настройки Kadro, которые хранятся в `NSUserDefaults`.

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

/// Настройка с выбором из нескольких вариантов: хранится строкой и показывается радиокнопками.
pub trait Choice: Copy + PartialEq + 'static {
    /// Ключ настройки в `NSUserDefaults`.
    const DEFAULTS_KEY: &'static str;
    /// Все варианты в порядке радиокнопок в настройках.
    const ALL: &'static [Self];

    /// Подпись радиокнопки.
    fn title(self) -> &'static str;

    /// Значение варианта в `NSUserDefaults`.
    fn key(self) -> &'static str;

    /// Вариант по сохранённому значению. `None`, если значение незнакомо.
    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|choice| choice.key() == key)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Format {
    Png,
    Jpeg,
    Heic,
    /// PNG или JPEG в зависимости от содержимого снимка.
    Auto,
}

impl Format {
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

impl Choice for Format {
    const DEFAULTS_KEY: &'static str = FORMAT;
    const ALL: &'static [Self] = &[Format::Png, Format::Jpeg, Format::Heic, Format::Auto];

    fn title(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Jpeg => "JPEG",
            Format::Heic => "HEIC",
            Format::Auto => "Auto",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpeg",
            Format::Heic => "heic",
            Format::Auto => "auto",
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

impl Choice for Cursor {
    const DEFAULTS_KEY: &'static str = CURSOR;
    const ALL: &'static [Self] = &[Cursor::Auto, Cursor::Show, Cursor::Hide];

    fn title(self) -> &'static str {
        match self {
            Cursor::Auto => "Auto",
            Cursor::Show => "Show",
            Cursor::Hide => "Hide",
        }
    }

    fn key(self) -> &'static str {
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

impl Choice for PreviewHiding {
    const DEFAULTS_KEY: &'static str = PREVIEW_HIDING;
    const ALL: &'static [Self] = &[PreviewHiding::Auto, PreviewHiding::Manual];

    fn title(self) -> &'static str {
        match self {
            PreviewHiding::Auto => "Automatically",
            PreviewHiding::Manual => "Manually",
        }
    }

    fn key(self) -> &'static str {
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

impl Choice for Confirmation {
    const DEFAULTS_KEY: &'static str = CONFIRMATION;
    const ALL: &'static [Self] = &[Confirmation::Popup, Confirmation::Notification, Confirmation::None];

    fn title(self) -> &'static str {
        match self {
            Confirmation::Popup => "Pop-up",
            Confirmation::Notification => "Notification",
            Confirmation::None => "None",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Confirmation::Popup => "popup",
            Confirmation::Notification => "notification",
            Confirmation::None => "none",
        }
    }
}

/// Пользовательские настройки из `NSUserDefaults`.
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
        let save_to_file = flag(SAVE_TO_FILE).unwrap_or(true);
        let show_preview = flag(SHOW_PREVIEW).unwrap_or(true);
        // Хотя бы одно действие после снимка включено всегда, иначе снимок пропадёт.
        let copy_to_clipboard = flag(COPY_TO_CLIPBOARD).unwrap_or(true) || (!save_to_file && !show_preview);
        let directory = string(DIRECTORY)
            .map_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Desktop"), PathBuf::from);
        let file_name_template = string(FILE_NAME_TEMPLATE)
            .filter(|template| !template.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_FILE_NAME_TEMPLATE.into());

        Settings {
            copy_to_clipboard,
            save_to_file,
            format: choice().unwrap_or(Format::Png),
            directory,
            downscale_retina: flag(DOWNSCALE_RETINA).unwrap_or(false),
            cursor: choice().unwrap_or(Cursor::Auto),
            file_name_template,
            show_preview,
            preview_hiding: choice().unwrap_or(PreviewHiding::Auto),
            confirmation: choice().unwrap_or(Confirmation::Popup),
        }
    }
}

/// Сохранённый флаг. `None`, если пользователь его ещё не менял.
fn flag(key: &str) -> Option<bool> {
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str(key);
    defaults.objectForKey(&key).map(|_| defaults.boolForKey(&key))
}

pub fn string(key: &str) -> Option<String> {
    NSUserDefaults::standardUserDefaults().stringForKey(&NSString::from_str(key)).map(|s| s.to_string())
}

/// Сохранённый вариант настройки. `None`, если его нет или значение незнакомо.
pub fn choice<T: Choice>() -> Option<T> {
    string(T::DEFAULTS_KEY).and_then(|key| T::from_key(&key))
}

pub fn set_flag(key: &str, value: bool) {
    let key = NSString::from_str(key);
    unsafe { NSUserDefaults::standardUserDefaults().setObject_forKey(Some(&NSNumber::new_bool(value)), &key) };
}

pub fn set_string(key: &str, value: &str) {
    let key = NSString::from_str(key);
    unsafe { NSUserDefaults::standardUserDefaults().setObject_forKey(Some(&NSString::from_str(value)), &key) };
}

pub fn set_choice<T: Choice>(choice: T) {
    set_string(T::DEFAULTS_KEY, choice.key());
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn assert_keys_round_trip<T: Choice + std::fmt::Debug>() {
        let keys: HashSet<_> = T::ALL.iter().map(|choice| choice.key()).collect();
        assert_eq!(keys.len(), T::ALL.len(), "keys of {} must be unique", T::DEFAULTS_KEY);
        for choice in T::ALL {
            assert_eq!(T::from_key(choice.key()), Some(*choice));
        }
        assert_eq!(T::from_key("unknown"), None);
    }

    #[test]
    fn every_choice_is_restored_from_its_stored_key() {
        assert_keys_round_trip::<Format>();
        assert_keys_round_trip::<Cursor>();
        assert_keys_round_trip::<PreviewHiding>();
        assert_keys_round_trip::<Confirmation>();
    }
}
