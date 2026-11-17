//! Bridges between Core Foundation and Foundation.
//!
//! Many Core Foundation and Foundation types are toll-free bridged:
//! `CFArray` and `NSArray`, `CFDictionary` and `NSDictionary`, `CFString` and `NSString`,
//! `CTFontDescriptor` and `NSFontDescriptor`. These casts live here so their safety
//! condition is written down in one place.

use std::ptr;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSDictionary, NSNumber, NSString};

/// Views an object of one type as the toll-free bridged object of another type.
///
/// # Safety
///
/// `T` and `U` must be a toll-free bridged pair, and the object must really be of type `U`
/// as far as Objective-C is concerned. The result does not outlive `value`.
pub unsafe fn toll_free<T: ?Sized, U>(value: &T) -> &U {
    // SAFETY: by the function contract the pointer holds a `U` object, and the lifetime is preserved.
    unsafe { &*ptr::from_ref(value).cast::<U>() }
}

/// Number from a Window Server or Carbon dictionary. `None` if the key is missing or not a number.
pub fn number(info: &NSDictionary<NSString, AnyObject>, key: &NSString) -> Option<Retained<NSNumber>> {
    info.objectForKey(key)?.downcast::<NSNumber>().ok()
}
