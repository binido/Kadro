//! Глобальные сочетания клавиш через Carbon `RegisterEventHotKey`.
//!
//! Сочетания хранятся в `NSUserDefaults` и регистрируются заново после каждого изменения в настройках.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::{self, NonNull, null_mut};

use objc2::MainThreadMarker;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSEventModifierFlags;
use objc2_core_foundation::{CFArray, CFRetained};
use objc2_foundation::{NSArray, NSDictionary, NSString, NSUserDefaults, ns_string};

use crate::bridge::{self, number};
use crate::{browser, capture, editor, output, overlay, pin, upload};

type OSStatus = i32;

/// `noErr` из Carbon: вызов прошёл успешно.
const NO_ERR: OSStatus = 0;

#[repr(C)]
#[derive(Default)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

type EventHandler = extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> OSStatus;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> *mut c_void;
    fn InstallEventHandler(
        target: *mut c_void,
        handler: EventHandler,
        num_types: u32,
        list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut *mut c_void,
    ) -> OSStatus;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyID,
        target: *mut c_void,
        options: u32,
        out_ref: *mut *mut c_void,
    ) -> OSStatus;
    fn UnregisterEventHotKey(hot_key: *mut c_void) -> OSStatus;
    fn GetEventParameter(
        event: *mut c_void,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> OSStatus;
    fn CopySymbolicHotKeys(out: *mut *const CFArray) -> OSStatus;
}

/// Четырёхбуквенный код Carbon, как `'KDRO'` в C.
const fn four_cc(code: [u8; 4]) -> u32 {
    u32::from_be_bytes(code)
}

const SIGNATURE: u32 = four_cc(*b"KDRO");
const KEYBOARD_EVENT_CLASS: u32 = four_cc(*b"keyb");
const HOT_KEY_PRESSED: u32 = 5;
const DIRECT_OBJECT_PARAM: u32 = four_cc(*b"----");
const HOT_KEY_ID_TYPE: u32 = four_cc(*b"hkid");
// С этим флагом регистрация падает, если сочетание уже занято другим приложением.
const EXCLUSIVE: u32 = 1;

// Модификаторы в формате Carbon, в нём же macOS отдаёт системные сочетания.
const CMD: u32 = 0x100;
const SHIFT: u32 = 0x200;
const OPTION: u32 = 0x800;
const CONTROL: u32 = 0x1000;

/// Коды клавиш по раскладке ANSI. Буквы показываются латиницей при любой раскладке, как в самой macOS.
#[rustfmt::skip]
const KEYS: &[(u32, &str)] = &[
    (0, "A"), (1, "S"), (2, "D"), (3, "F"), (4, "H"), (5, "G"), (6, "Z"), (7, "X"), (8, "C"),
    (9, "V"), (11, "B"), (12, "Q"), (13, "W"), (14, "E"), (15, "R"), (16, "Y"), (17, "T"),
    (18, "1"), (19, "2"), (20, "3"), (21, "4"), (22, "6"), (23, "5"), (24, "="), (25, "9"),
    (26, "7"), (27, "-"), (28, "8"), (29, "0"), (30, "]"), (31, "O"), (32, "U"), (33, "["),
    (34, "I"), (35, "P"), (37, "L"), (38, "J"), (39, "'"), (40, "K"), (41, ";"), (42, "\\"),
    (43, ","), (44, "/"), (45, "N"), (46, "M"), (47, "."), (50, "`"),
    (36, "↩"), (48, "⇥"), (49, "Space"), (51, "⌫"), (117, "⌦"),
    (115, "Home"), (119, "End"), (116, "Page Up"), (121, "Page Down"),
    (123, "←"), (124, "→"), (125, "↓"), (126, "↑"),
    (122, "F1"), (120, "F2"), (99, "F3"), (118, "F4"), (96, "F5"), (97, "F6"), (98, "F7"),
    (100, "F8"), (101, "F9"), (109, "F10"), (103, "F11"), (111, "F12"),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Shortcut {
    pub key_code: u32,
    pub modifiers: u32,
}

impl Shortcut {
    pub fn from_event(key_code: u16, flags: NSEventModifierFlags) -> Self {
        let mut modifiers = 0;
        for (flag, carbon) in [
            (NSEventModifierFlags::Command, CMD),
            (NSEventModifierFlags::Shift, SHIFT),
            (NSEventModifierFlags::Option, OPTION),
            (NSEventModifierFlags::Control, CONTROL),
        ] {
            if flags.contains(flag) {
                modifiers |= carbon;
            }
        }
        Shortcut { key_code: key_code.into(), modifiers }
    }

    /// Без ⌘, ⌃ или ⌥ сочетание перехватывало бы обычный ввод текста.
    pub fn is_usable(self) -> bool {
        self.modifiers & (CMD | CONTROL | OPTION) != 0
    }

    pub fn key_name(self) -> Option<&'static str> {
        KEYS.iter().find(|(code, _)| *code == self.key_code).map(|(_, name)| *name)
    }

    pub fn display(self) -> String {
        let mut text = String::new();
        for (bit, symbol) in [(CONTROL, "⌃"), (OPTION, "⌥"), (SHIFT, "⇧"), (CMD, "⌘")] {
            if self.modifiers & bit != 0 {
                text.push_str(symbol);
            }
        }
        match self.key_name() {
            Some(name) => text + name,
            None => format!("{text}Key {}", self.key_code),
        }
    }

    /// Флаги модификаторов для keyEquivalent пункта меню.
    pub fn menu_modifiers(self) -> NSEventModifierFlags {
        let mut flags = NSEventModifierFlags::empty();
        for (carbon, flag) in [
            (CMD, NSEventModifierFlags::Command),
            (SHIFT, NSEventModifierFlags::Shift),
            (OPTION, NSEventModifierFlags::Option),
            (CONTROL, NSEventModifierFlags::Control),
        ] {
            if self.modifiers & carbon != 0 {
                flags |= flag;
            }
        }
        flags
    }

    fn encode(self) -> String {
        format!("{}:{}", self.key_code, self.modifiers)
    }

    fn decode(text: &str) -> Option<Self> {
        let (key_code, modifiers) = text.split_once(':')?;
        Some(Shortcut { key_code: key_code.parse().ok()?, modifiers: modifiers.parse().ok()? })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Region,
    RepeatRegion,
    Screen,
    DelayedScreen,
    Window,
    ActiveWindow,
    PinLast,
    EditLast,
    RecognizeText,
    ScrollCapture,
    UploadLast,
    OpenLibrary,
}

impl Action {
    /// Порядок пунктов в меню и строк в настройках.
    pub const ALL: [Action; 12] = [
        Action::Region,
        Action::RepeatRegion,
        Action::Screen,
        Action::DelayedScreen,
        Action::Window,
        Action::ActiveWindow,
        Action::ScrollCapture,
        Action::PinLast,
        Action::EditLast,
        Action::UploadLast,
        Action::OpenLibrary,
        Action::RecognizeText,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Action::Region => "Capture Region",
            Action::RepeatRegion => "Repeat Last Region",
            Action::Screen => "Capture Full Screen",
            Action::DelayedScreen => "Capture Full Screen in 3 s",
            Action::Window => "Capture Window",
            Action::ActiveWindow => "Capture Active Window",
            Action::PinLast => "Pin Last Screenshot",
            Action::EditLast => "Edit Last Screenshot",
            Action::RecognizeText => "Recognize Text",
            Action::ScrollCapture => "Scrolling Capture",
            Action::UploadLast => "Upload Last Screenshot",
            Action::OpenLibrary => "Library",
        }
    }

    pub fn run(self, mtm: MainThreadMarker) {
        match self {
            Action::Region => {
                overlay::select(mtm, overlay::Mode::Region);
            }
            Action::RepeatRegion => capture::repeat_area(mtm),
            Action::Screen => capture::capture_screen(mtm),
            Action::DelayedScreen => capture::capture_delayed(mtm),
            Action::Window => {
                overlay::select(mtm, overlay::Mode::Window);
            }
            Action::ActiveWindow => capture::capture_active_window(mtm),
            Action::PinLast => pin::pin_last(mtm),
            Action::EditLast => editor::open_last(mtm),
            Action::UploadLast => upload::upload_last(mtm),
            Action::OpenLibrary => browser::show(mtm),
            Action::ScrollCapture => {
                output::route_next(output::Route::Scroll);
                if !overlay::select(mtm, overlay::Mode::Region) {
                    output::cancel_route();
                }
            }
            Action::RecognizeText => {
                output::route_next(output::Route::Text);
                if !overlay::select(mtm, overlay::Mode::Region) {
                    output::cancel_route();
                }
            }
        }
    }

    /// Номер действия для Carbon, по нему обработчик находит действие в ALL.
    fn index(self) -> u32 {
        Action::ALL.iter().position(|a| *a == self).unwrap_or(0) as u32
    }

    fn defaults_key(self) -> &'static str {
        match self {
            Action::Region => "shortcutRegion",
            Action::RepeatRegion => "shortcutRepeatRegion",
            Action::Screen => "shortcutScreen",
            Action::DelayedScreen => "shortcutDelayedScreen",
            Action::Window => "shortcutWindow",
            Action::ActiveWindow => "shortcutActiveWindow",
            Action::PinLast => "shortcutPinLast",
            Action::EditLast => "shortcutEditLast",
            Action::RecognizeText => "shortcutRecognizeText",
            Action::ScrollCapture => "shortcutScrollCapture",
            Action::UploadLast => "shortcutUploadLast",
            Action::OpenLibrary => "shortcutLibrary",
        }
    }

    fn default_shortcut(self) -> Option<Shortcut> {
        // ⌃⇧1, ⌃⇧2, ⌃⇧3 - системные ⌘⇧3 и ⌘⇧4 остаются за macOS.
        let key_code = match self {
            Action::Screen => 18,
            Action::Region => 19,
            Action::Window => 20,
            Action::RepeatRegion
            | Action::DelayedScreen
            | Action::ActiveWindow
            | Action::PinLast
            | Action::EditLast
            | Action::RecognizeText
            | Action::ScrollCapture
            | Action::UploadLast
            | Action::OpenLibrary => {
                return None;
            }
        };
        Some(Shortcut { key_code, modifiers: CONTROL | SHIFT })
    }

    /// Текущее сочетание. Пустая строка в настройках означает, что пользователь его снял.
    pub fn shortcut(self) -> Option<Shortcut> {
        let stored = NSUserDefaults::standardUserDefaults().stringForKey(&NSString::from_str(self.defaults_key()));
        match stored {
            None => self.default_shortcut(),
            Some(text) => Shortcut::decode(&text.to_string()),
        }
    }

    pub fn set_shortcut(self, shortcut: Option<Shortcut>) {
        let value = NSString::from_str(&shortcut.map(Shortcut::encode).unwrap_or_default());
        let key = NSString::from_str(self.defaults_key());
        unsafe { NSUserDefaults::standardUserDefaults().setObject_forKey(Some(&value), &key) };
    }
}

thread_local! {
    static REGISTERED: RefCell<Vec<*mut c_void>> = const { RefCell::new(Vec::new()) };
}

/// Подключает обработчик горячих клавиш и регистрирует сохранённые сочетания.
pub fn install() {
    let spec = EventTypeSpec { event_class: KEYBOARD_EVENT_CLASS, event_kind: HOT_KEY_PRESSED };
    // SAFETY: spec живёт до конца вызова, а Carbon копирует список типов событий себе.
    let status = unsafe {
        InstallEventHandler(GetApplicationEventTarget(), handle_hot_key, 1, &raw const spec, null_mut(), null_mut())
    };
    if status != NO_ERR {
        eprintln!("kadro: failed to install hot key handler: OSStatus {status}");
    }
    register_all();
}

pub fn register_all() {
    unregister_all();
    for action in Action::ALL {
        if let Some(shortcut) = action.shortcut() {
            match register(action, shortcut) {
                Some(hot_key) => REGISTERED.with_borrow_mut(|r| r.push(hot_key)),
                None => eprintln!("kadro: {} is taken by another app", shortcut.display()),
            }
        }
    }
}

/// Снимает все сочетания, иначе при записи нового нажатие текущего запускало бы захват.
pub fn unregister_all() {
    for hot_key in REGISTERED.take() {
        // SAFETY: в REGISTERED лежат только ссылки, которые вернул успешный RegisterEventHotKey.
        unsafe { UnregisterEventHotKey(hot_key) };
    }
}

fn register(action: Action, shortcut: Shortcut) -> Option<*mut c_void> {
    let id = EventHotKeyID { signature: SIGNATURE, id: action.index() };
    let mut hot_key = null_mut();
    // SAFETY: hot_key - локальная переменная, куда Carbon пишет ссылку на зарегистрированное сочетание.
    let status = unsafe {
        RegisterEventHotKey(
            shortcut.key_code,
            shortcut.modifiers,
            id,
            GetApplicationEventTarget(),
            EXCLUSIVE,
            &raw mut hot_key,
        )
    };
    (status == NO_ERR).then_some(hot_key)
}

/// Проверяет сочетание для действия. Возвращает текст конфликта для пользователя.
pub fn conflict(action: Action, shortcut: Shortcut) -> Option<String> {
    if let Some(other) = Action::ALL.into_iter().find(|a| *a != action && a.shortcut() == Some(shortcut)) {
        return Some(format!("Already used for {}.", other.title()));
    }
    if system_shortcuts().contains(&shortcut) {
        return Some("Used by a macOS keyboard shortcut.".into());
    }
    // Пробная регистрация - единственный способ узнать, занято ли сочетание другим приложением.
    match register(action, shortcut) {
        Some(hot_key) => {
            // SAFETY: hot_key только что вернул успешный RegisterEventHotKey.
            unsafe { UnregisterEventHotKey(hot_key) };
            None
        }
        None => Some("Used by another app.".into()),
    }
}

/// Включённые системные сочетания из Системных настроек -> Клавиатура.
fn system_shortcuts() -> Vec<Shortcut> {
    let mut list: *const CFArray = ptr::null();
    // SAFETY: list - локальная переменная для выходного параметра.
    if unsafe { CopySymbolicHotKeys(&raw mut list) } != NO_ERR {
        return Vec::new();
    }
    // SAFETY: по правилу Copy из Core Foundation владение массивом переходит к нам.
    let Some(list) = NonNull::new(list.cast_mut()).map(|p| unsafe { CFRetained::from_raw(p) }) else {
        return Vec::new();
    };
    // SAFETY: CFArray и NSArray взаимозаменяемы, а элементы массива - словари с описанием сочетаний.
    let list: &NSArray<NSDictionary<NSString, AnyObject>> = unsafe { bridge::toll_free(&*list) };
    list.iter()
        .filter_map(|info| {
            if !number(&info, ns_string!("kHISymbolicHotKeyEnabled"))?.boolValue() {
                return None;
            }
            Some(Shortcut {
                key_code: number(&info, ns_string!("kHISymbolicHotKeyCode"))?.unsignedIntValue(),
                modifiers: number(&info, ns_string!("kHISymbolicHotKeyModifiers"))?.unsignedIntValue()
                    & (CMD | SHIFT | OPTION | CONTROL),
            })
        })
        .collect()
}

extern "C" fn handle_hot_key(_call: *mut c_void, event: *mut c_void, _data: *mut c_void) -> OSStatus {
    let mut id = EventHotKeyID::default();
    // SAFETY: event пришёл от Carbon, а буфер id нужного размера и типа для параметра hkid.
    let status = unsafe {
        GetEventParameter(
            event,
            DIRECT_OBJECT_PARAM,
            HOT_KEY_ID_TYPE,
            null_mut(),
            size_of::<EventHotKeyID>(),
            null_mut(),
            ptr::from_mut(&mut id).cast(),
        )
    };
    // Carbon вызывает обработчик на главном потоке, из цикла событий NSApplication.
    let action = usize::try_from(id.id).ok().and_then(|index| Action::ALL.get(index));
    if let (NO_ERR, Some(action), Some(mtm)) = (status, action, MainThreadMarker::new()) {
        action.run(mtm);
    }
    NO_ERR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_round_trips_through_storage_and_shows_symbols() {
        let shortcut = Shortcut { key_code: 19, modifiers: CONTROL | SHIFT };
        assert_eq!(Shortcut::decode(&shortcut.encode()), Some(shortcut));
        assert_eq!(Shortcut::decode(""), None);
        assert_eq!(shortcut.display(), "⌃⇧2");
        assert!(shortcut.is_usable());
        assert!(!Shortcut { key_code: 0, modifiers: SHIFT }.is_usable());
    }
}
