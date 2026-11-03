//! Мосты между Core Foundation и Foundation.
//!
//! Многие типы Core Foundation и Foundation взаимозаменяемы без копирования (toll-free bridged):
//! `CFArray` и `NSArray`, `CFDictionary` и `NSDictionary`, `CFString` и `NSString`,
//! `CTFontDescriptor` и `NSFontDescriptor`. Здесь собраны такие приведения, чтобы условие
//! их безопасности было записано в одном месте.

use std::ptr;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSDictionary, NSNumber, NSString};

/// Смотрит на объект одного типа как на взаимозаменяемый с ним объект другого типа.
///
/// # Safety
///
/// `T` и `U` должны быть парой toll-free bridged типов, а сам объект - действительно иметь тип `U`
/// с точки зрения Objective-C. Время жизни результата не больше времени жизни `value`.
pub unsafe fn toll_free<T: ?Sized, U>(value: &T) -> &U {
    // SAFETY: по условию функции под указателем лежит объект типа `U`, а время жизни сохраняется.
    unsafe { &*ptr::from_ref(value).cast::<U>() }
}

/// Число из словаря, который отдают Window Server или Carbon. `None`, если ключа нет или там не число.
pub fn number(info: &NSDictionary<NSString, AnyObject>, key: &NSString) -> Option<Retained<NSNumber>> {
    info.objectForKey(key)?.downcast::<NSNumber>().ok()
}
