//! Виртуальные коды клавиш macOS, на которые Kadro реагирует напрямую.
//!
//! Коды не зависят от раскладки: они описывают физическое место клавиши на клавиатуре ANSI.

pub const S: u16 = 1;
pub const Z: u16 = 6;
pub const C: u16 = 8;
pub const W: u16 = 13;
pub const EQUAL: u16 = 24;
pub const NINE: u16 = 25;
pub const MINUS: u16 = 27;
pub const ZERO: u16 = 29;
pub const P: u16 = 35;
pub const RETURN: u16 = 36;
pub const DELETE: u16 = 51;
pub const ESCAPE: u16 = 53;
/// Enter на цифровой клавиатуре.
pub const ENTER: u16 = 76;
pub const FORWARD_DELETE: u16 = 117;
pub const LEFT: u16 = 123;
pub const RIGHT: u16 = 124;
pub const DOWN: u16 = 125;
pub const UP: u16 = 126;
