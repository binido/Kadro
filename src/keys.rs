//! Виртуальные коды клавиш macOS, на которые Kadro реагирует напрямую.
//!
//! Коды не зависят от раскладки: они описывают физическое место клавиши на клавиатуре ANSI.

pub const C: u16 = 8;
pub const DELETE: u16 = 51;
pub const ESCAPE: u16 = 53;
pub const FORWARD_DELETE: u16 = 117;
pub const LEFT: u16 = 123;
pub const RIGHT: u16 = 124;
pub const DOWN: u16 = 125;
pub const UP: u16 = 126;
