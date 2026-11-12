//! Пользовательские настройки Kadro, которые хранятся в `NSUserDefaults`.

use std::path::PathBuf;

use objc2_foundation::{NSNumber, NSString, NSUserDefaults};

use crate::annotation::Color;

pub const COPY_TO_CLIPBOARD: &str = "copyToClipboard";
pub const SAVE_TO_FILE: &str = "saveToFile";
pub const FORMAT: &str = "format";
pub const DIRECTORY: &str = "directory";
pub const DOWNSCALE_RETINA: &str = "downscaleRetina";
pub const CURSOR: &str = "cursor";
pub const FILE_NAME_TEMPLATE: &str = "fileNameTemplate";
pub const LAST_AREA: &str = "lastArea";
/// Старый флаг превью. Учитывается, только если `AFTER_CAPTURE` не сохранён.
pub const SHOW_PREVIEW: &str = "showPreview";
pub const AFTER_CAPTURE: &str = "afterCapture";
pub const EDITOR_ESC: &str = "editorEsc";
pub const EDITOR_ZOOM: &str = "editorZoom";
pub const EDITOR_ON_TOP: &str = "editorOnTop";
pub const EDITOR_SCROLL_ZOOM: &str = "editorScrollZoom";
pub const WINDOW_BACKGROUND: &str = "windowBackground";
pub const WINDOW_COLOR: &str = "windowBackgroundColor";
pub const RECOGNITION_LANGUAGE: &str = "recognitionLanguage";
pub const RECOGNITION_SINGLE_LINE: &str = "recognitionSingleLine";
pub const SCROLL_REVERSE: &str = "scrollReverse";
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

/// Что лежит за окном на снимке окна.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum WindowBackground {
    /// Только окно, без тени, со скруглёнными углами, достроенными до прямоугольника.
    Clean,
    /// Окно с тенью на прозрачном фоне.
    Shadow,
    /// Окно с тенью на обоях его экрана.
    Wallpaper,
    /// Окно с тенью на цвете из настроек.
    Color,
}

impl Choice for WindowBackground {
    const DEFAULTS_KEY: &'static str = WINDOW_BACKGROUND;
    const ALL: &'static [Self] =
        &[WindowBackground::Clean, WindowBackground::Shadow, WindowBackground::Wallpaper, WindowBackground::Color];

    fn title(self) -> &'static str {
        match self {
            WindowBackground::Clean => "No shadow",
            WindowBackground::Shadow => "Transparent",
            WindowBackground::Wallpaper => "Wallpaper",
            WindowBackground::Color => "Color",
        }
    }

    fn key(self) -> &'static str {
        match self {
            WindowBackground::Clean => "clean",
            WindowBackground::Shadow => "shadow",
            WindowBackground::Wallpaper => "wallpaper",
            WindowBackground::Color => "color",
        }
    }
}

/// Наибольшая высота склеенного снимка прокрутки.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ScrollLimit {
    Short,
    Medium,
    Long,
    Huge,
}

impl ScrollLimit {
    pub fn pixels(self) -> usize {
        match self {
            ScrollLimit::Short => 5_000,
            ScrollLimit::Medium => 10_000,
            ScrollLimit::Long => 20_000,
            ScrollLimit::Huge => 40_000,
        }
    }
}

impl Choice for ScrollLimit {
    const DEFAULTS_KEY: &'static str = "scrollLimit";
    const ALL: &'static [Self] = &[ScrollLimit::Short, ScrollLimit::Medium, ScrollLimit::Long, ScrollLimit::Huge];

    fn title(self) -> &'static str {
        match self {
            ScrollLimit::Short => "5 000 px",
            ScrollLimit::Medium => "10 000 px",
            ScrollLimit::Long => "20 000 px",
            ScrollLimit::Huge => "40 000 px",
        }
    }

    fn key(self) -> &'static str {
        match self {
            ScrollLimit::Short => "5000",
            ScrollLimit::Medium => "10000",
            ScrollLimit::Long => "20000",
            ScrollLimit::Huge => "40000",
        }
    }
}

/// Насколько далеко прокручивать за шаг автоматической прокрутки.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ScrollSpeed {
    Slow,
    Normal,
    Fast,
}

impl ScrollSpeed {
    /// Шаг как доля высоты области.
    pub fn step(self) -> f64 {
        match self {
            ScrollSpeed::Slow => 0.3,
            ScrollSpeed::Normal => 0.5,
            ScrollSpeed::Fast => 0.75,
        }
    }
}

impl Choice for ScrollSpeed {
    const DEFAULTS_KEY: &'static str = "scrollSpeed";
    const ALL: &'static [Self] = &[ScrollSpeed::Slow, ScrollSpeed::Normal, ScrollSpeed::Fast];

    fn title(self) -> &'static str {
        match self {
            ScrollSpeed::Slow => "Slow",
            ScrollSpeed::Normal => "Normal",
            ScrollSpeed::Fast => "Fast",
        }
    }

    fn key(self) -> &'static str {
        match self {
            ScrollSpeed::Slow => "slow",
            ScrollSpeed::Normal => "normal",
            ScrollSpeed::Fast => "fast",
        }
    }
}

pub const UPLOAD_ENDPOINT: &str = "uploadEndpoint";
pub const UPLOAD_REGION: &str = "uploadRegion";
pub const UPLOAD_BUCKET: &str = "uploadBucket";
pub const UPLOAD_PREFIX: &str = "uploadPrefix";
pub const UPLOAD_PUBLIC_URL: &str = "uploadPublicUrl";
pub const LIBRARY: &str = "library";
pub const HIDE_ICON: &str = "hideMenuBarIcon";
pub const CHECK_UPDATES: &str = "checkForUpdates";
pub const LAST_UPDATE_CHECK: &str = "lastUpdateCheck";
/// Номер вкладки, открытой в настройках последней.
pub const SETTINGS_TAB: &str = "settingsTab";

/// Какую ссылку копировать после загрузки.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LinkKind {
    /// Только загрузить.
    None,
    /// Подписанная ссылка, которая работает ограниченное время.
    Presigned,
    /// Постоянная ссылка на публичный бакет или его домен.
    Public,
}

impl Choice for LinkKind {
    const DEFAULTS_KEY: &'static str = "uploadLinks";
    const ALL: &'static [Self] = &[LinkKind::None, LinkKind::Presigned, LinkKind::Public];

    fn title(self) -> &'static str {
        match self {
            LinkKind::None => "No link",
            LinkKind::Presigned => "Temporary",
            LinkKind::Public => "Public",
        }
    }

    fn key(self) -> &'static str {
        match self {
            LinkKind::None => "none",
            LinkKind::Presigned => "presigned",
            LinkKind::Public => "public",
        }
    }
}

/// Сколько работает временная ссылка. S3 разрешает не больше 7 дней.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LinkExpiry {
    Hour,
    Day,
    Week,
}

impl LinkExpiry {
    pub fn seconds(self) -> u64 {
        match self {
            LinkExpiry::Hour => 3_600,
            LinkExpiry::Day => 86_400,
            LinkExpiry::Week => 604_800,
        }
    }
}

impl Choice for LinkExpiry {
    const DEFAULTS_KEY: &'static str = "uploadExpiry";
    const ALL: &'static [Self] = &[LinkExpiry::Hour, LinkExpiry::Day, LinkExpiry::Week];

    fn title(self) -> &'static str {
        match self {
            LinkExpiry::Hour => "1 hour",
            LinkExpiry::Day => "1 day",
            LinkExpiry::Week => "7 days",
        }
    }

    fn key(self) -> &'static str {
        match self {
            LinkExpiry::Hour => "hour",
            LinkExpiry::Day => "day",
            LinkExpiry::Week => "week",
        }
    }
}

/// Сколько снимков помнит библиотека. Старые выпадают из индекса, файлы остаются.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LibraryLimit {
    Thousand,
    FiveThousand,
    TwentyThousand,
    Unlimited,
}

impl LibraryLimit {
    pub fn items(self) -> usize {
        match self {
            LibraryLimit::Thousand => 1_000,
            LibraryLimit::FiveThousand => 5_000,
            LibraryLimit::TwentyThousand => 20_000,
            LibraryLimit::Unlimited => usize::MAX,
        }
    }
}

impl Choice for LibraryLimit {
    const DEFAULTS_KEY: &'static str = "libraryLimit";
    const ALL: &'static [Self] =
        &[LibraryLimit::Thousand, LibraryLimit::FiveThousand, LibraryLimit::TwentyThousand, LibraryLimit::Unlimited];

    fn title(self) -> &'static str {
        match self {
            LibraryLimit::Thousand => "1 000",
            LibraryLimit::FiveThousand => "5 000",
            LibraryLimit::TwentyThousand => "20 000",
            LibraryLimit::Unlimited => "No limit",
        }
    }

    fn key(self) -> &'static str {
        match self {
            LibraryLimit::Thousand => "1000",
            LibraryLimit::FiveThousand => "5000",
            LibraryLimit::TwentyThousand => "20000",
            LibraryLimit::Unlimited => "unlimited",
        }
    }
}

/// Что делать со ссылками `kadro://`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LinkPolicy {
    Off,
    /// Спрашивать перед каждой ссылкой.
    Ask,
    Allow,
}

impl Choice for LinkPolicy {
    const DEFAULTS_KEY: &'static str = "urlScheme";
    const ALL: &'static [Self] = &[LinkPolicy::Off, LinkPolicy::Ask, LinkPolicy::Allow];

    fn title(self) -> &'static str {
        match self {
            LinkPolicy::Off => "Off",
            LinkPolicy::Ask => "Ask first",
            LinkPolicy::Allow => "Allow",
        }
    }

    fn key(self) -> &'static str {
        match self {
            LinkPolicy::Off => "off",
            LinkPolicy::Ask => "ask",
            LinkPolicy::Allow => "allow",
        }
    }
}

/// Цвет из записи `#RRGGBB`.
pub fn parse_color(text: &str) -> Option<Color> {
    let digits = text.strip_prefix('#')?;
    let rgb = u32::from_str_radix(digits, 16).ok().filter(|_| digits.len() == 6)?;
    let channel = |shift: u32| f64::from((rgb >> shift) & 0xff) / 255.0;
    Some(Color { red: channel(16), green: channel(8), blue: channel(0), alpha: 1.0 })
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

/// Что показать после снимка.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum AfterCapture {
    Preview,
    Editor,
    Nothing,
}

impl Choice for AfterCapture {
    const DEFAULTS_KEY: &'static str = AFTER_CAPTURE;
    const ALL: &'static [Self] = &[AfterCapture::Preview, AfterCapture::Editor, AfterCapture::Nothing];

    fn title(self) -> &'static str {
        match self {
            AfterCapture::Preview => "Preview",
            AfterCapture::Editor => "Editor",
            AfterCapture::Nothing => "Nothing",
        }
    }

    fn key(self) -> &'static str {
        match self {
            AfterCapture::Preview => "preview",
            AfterCapture::Editor => "editor",
            AfterCapture::Nothing => "nothing",
        }
    }
}

/// Что делает Esc в редакторе, кроме закрытия.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum EscAction {
    Copy,
    Save,
    Close,
}

impl Choice for EscAction {
    const DEFAULTS_KEY: &'static str = EDITOR_ESC;
    const ALL: &'static [Self] = &[EscAction::Copy, EscAction::Save, EscAction::Close];

    fn title(self) -> &'static str {
        match self {
            EscAction::Copy => "Copy and close",
            EscAction::Save => "Save and close",
            EscAction::Close => "Just close",
        }
    }

    fn key(self) -> &'static str {
        match self {
            EscAction::Copy => "copy",
            EscAction::Save => "save",
            EscAction::Close => "close",
        }
    }
}

/// Масштаб снимка при открытии редактора.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DefaultZoom {
    /// Целиком в окне.
    Fit,
    /// 100%, если снимок помещается, иначе целиком в окне.
    Actual,
}

impl Choice for DefaultZoom {
    const DEFAULTS_KEY: &'static str = EDITOR_ZOOM;
    const ALL: &'static [Self] = &[DefaultZoom::Fit, DefaultZoom::Actual];

    fn title(self) -> &'static str {
        match self {
            DefaultZoom::Fit => "Fit to window",
            DefaultZoom::Actual => "Prefer 100%",
        }
    }

    fn key(self) -> &'static str {
        match self {
            DefaultZoom::Fit => "fit",
            DefaultZoom::Actual => "actual",
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
    pub after_capture: AfterCapture,
    pub preview_hiding: PreviewHiding,
    pub confirmation: Confirmation,
    pub esc_action: EscAction,
    pub default_zoom: DefaultZoom,
    pub editor_on_top: bool,
    pub scroll_zoom: bool,
    pub window_background: WindowBackground,
    pub window_color: Color,
    /// Язык распознавания текста. None - языки системы.
    pub recognition_language: Option<String>,
    /// Склеивать распознанные строки в одну.
    pub single_line: bool,
    pub scroll_limit: ScrollLimit,
    pub scroll_speed: ScrollSpeed,
    /// Крутить в обратную сторону, для приложений с перевёрнутой прокруткой.
    pub scroll_reverse: bool,
    /// Вести библиотеку сохранённых снимков.
    pub library: bool,
    pub library_limit: LibraryLimit,
    pub links: LinkPolicy,
    /// Не показывать иконку в строке меню.
    pub hide_icon: bool,
    /// Раз в день проверять, вышла ли новая версия.
    pub check_updates: bool,
}

impl Settings {
    /// Читает настройки, подставляя значения по умолчанию для отсутствующих ключей.
    pub fn load() -> Self {
        let save_to_file = flag(SAVE_TO_FILE).unwrap_or(true);
        let after_capture = choice().unwrap_or(match flag(SHOW_PREVIEW) {
            Some(false) => AfterCapture::Nothing,
            _ => AfterCapture::Preview,
        });
        // Хотя бы одно действие после снимка включено всегда, иначе снимок пропадёт.
        let copy_to_clipboard =
            flag(COPY_TO_CLIPBOARD).unwrap_or(true) || (!save_to_file && after_capture == AfterCapture::Nothing);
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
            after_capture,
            preview_hiding: choice().unwrap_or(PreviewHiding::Auto),
            confirmation: choice().unwrap_or(Confirmation::Popup),
            esc_action: choice().unwrap_or(EscAction::Copy),
            default_zoom: choice().unwrap_or(DefaultZoom::Actual),
            editor_on_top: flag(EDITOR_ON_TOP).unwrap_or(false),
            scroll_zoom: flag(EDITOR_SCROLL_ZOOM).unwrap_or(false),
            window_background: choice().unwrap_or(WindowBackground::Clean),
            window_color: string(WINDOW_COLOR).and_then(|text| parse_color(&text)).unwrap_or(Color {
                red: 1.0,
                green: 1.0,
                blue: 1.0,
                alpha: 1.0,
            }),
            recognition_language: string(RECOGNITION_LANGUAGE).filter(|language| !language.is_empty()),
            single_line: flag(RECOGNITION_SINGLE_LINE).unwrap_or(false),
            scroll_limit: choice().unwrap_or(ScrollLimit::Long),
            scroll_speed: choice().unwrap_or(ScrollSpeed::Normal),
            scroll_reverse: flag(SCROLL_REVERSE).unwrap_or(false),
            library: flag(LIBRARY).unwrap_or(true),
            library_limit: choice().unwrap_or(LibraryLimit::FiveThousand),
            links: choice().unwrap_or(LinkPolicy::Off),
            hide_icon: flag(HIDE_ICON).unwrap_or(false),
            check_updates: flag(CHECK_UPDATES).unwrap_or(true),
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
        assert_keys_round_trip::<AfterCapture>();
        assert_keys_round_trip::<EscAction>();
        assert_keys_round_trip::<DefaultZoom>();
        assert_keys_round_trip::<WindowBackground>();
        assert_keys_round_trip::<ScrollLimit>();
        assert_keys_round_trip::<ScrollSpeed>();
        assert_keys_round_trip::<LinkKind>();
        assert_keys_round_trip::<LinkExpiry>();
        assert_keys_round_trip::<LibraryLimit>();
        assert_keys_round_trip::<LinkPolicy>();
    }

    #[test]
    fn window_colour_is_read_from_hex() {
        let color = parse_color("#3366CC").unwrap();
        assert_eq!((color.red, color.green, color.blue), (0.2, 0.4, 0.8));
        assert_eq!(parse_color("336699"), None);
        assert_eq!(parse_color("#12345"), None);
    }
}
