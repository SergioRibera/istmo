//! macOS power plumbing shared by the battery and power devices
//! samplers: `IOKit` power sources, `IOKit` HID accessory batteries and
//! the Objective-C helpers they read `CoreFoundation` values with
//! (`CFDictionary` / `CFString` / `CFNumber` are toll-free bridged to
//! their Foundation classes).
//!
//! Every function runs inside its own autorelease pool: the `NSString`
//! keys are autoreleased, and samplers tick from plain threads.

use std::ffi::{CStr, c_char, c_void};

use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject};

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPSCopyPowerSourcesInfo() -> *const c_void;
    fn IOPSCopyPowerSourcesList(blob: *const c_void) -> *const c_void;
    fn IOPSGetPowerSourceDescription(blob: *const c_void, source: *const c_void) -> *const c_void;
    fn IOPSGetProvidingPowerSourceType(blob: *const c_void) -> *const c_void;
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(
        main_port: u32,
        matching: *mut c_void,
        existing: *mut u32,
    ) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: *const c_void,
        allocator: *const c_void,
        options: u32,
    ) -> *const c_void;
    fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

/// `kIOMainPortDefault`.
const MAIN_PORT_DEFAULT: u32 = 0;
/// `KERN_SUCCESS`.
const KERN_SUCCESS: i32 = 0;

/// One `IOPSGetPowerSourceDescription` entry.
#[derive(Debug, Clone, Default)]
pub(crate) struct PowerSource {
    /// `Type`: `InternalBattery`, `UPS`, …
    pub(crate) kind: String,
    /// `Name`.
    pub(crate) name: Option<String>,
    /// `Power Source ID`.
    pub(crate) id: Option<i64>,
    /// `Is Present`, `true` when missing.
    pub(crate) present: bool,
    /// `Current Capacity`.
    pub(crate) current: Option<i64>,
    /// `Max Capacity`.
    pub(crate) max: Option<i64>,
    /// `Is Charging`.
    pub(crate) charging: Option<bool>,
    /// `Is Charged`.
    pub(crate) charged: Option<bool>,
    /// `Power Source State` is `AC Power`.
    pub(crate) on_ac: Option<bool>,
}

/// Every power source and what powers the machine now
/// (`IOPSGetProvidingPowerSourceType`: `AC Power`, `Battery Power`,
/// `UPS Power`).
#[derive(Debug, Clone, Default)]
pub(crate) struct PowerSources {
    pub(crate) sources: Vec<PowerSource>,
    pub(crate) providing: Option<String>,
}

/// The `IOKit` power sources, or `None` when `IOKit` gives no snapshot.
pub(crate) fn power_sources() -> Option<PowerSources> {
    objc2::rc::autoreleasepool(|_| {
        // SAFETY: returns a +1 CFTypeRef (or NULL) released below.
        let blob = unsafe { IOPSCopyPowerSourcesInfo() };
        if blob.is_null() {
            return None;
        }
        let mut sources = Vec::new();
        // SAFETY: `blob` is the live info blob; the list is a +1
        // CFArrayRef (bridged to NSArray) released below.
        let list = unsafe { IOPSCopyPowerSourcesList(blob) };
        if !list.is_null() {
            let array = list.cast::<AnyObject>();
            // SAFETY: NSArray read on a live array.
            let count: usize = unsafe { msg_send![array, count] };
            for index in 0..count {
                // SAFETY: `index < count`; the element is owned by `list`.
                let source: *const AnyObject = unsafe { msg_send![array, objectAtIndex: index] };
                // SAFETY: a "Get" function: the dictionary is owned by
                // `blob`, which outlives this loop.
                let description = unsafe { IOPSGetPowerSourceDescription(blob, source.cast()) };
                // SAFETY: a CFDictionaryRef or NULL, read while `blob` lives.
                if let Some(source) = unsafe { power_source(description.cast()) } {
                    sources.push(source);
                }
            }
            // SAFETY: balances the Copy above.
            unsafe { CFRelease(list) };
        }
        // SAFETY: "Get" function on the live blob; the string is owned by
        // it and copied out before the release below.
        let providing = unsafe { string(IOPSGetProvidingPowerSourceType(blob).cast()) };
        // SAFETY: balances `IOPSCopyPowerSourcesInfo`.
        unsafe { CFRelease(blob) };
        Some(PowerSources { sources, providing })
    })
}

/// # Safety
///
/// `description` is NULL or a live `NSDictionary`; inside a pool.
unsafe fn power_source(description: *const AnyObject) -> Option<PowerSource> {
    if description.is_null() {
        return None;
    }
    // SAFETY: forwarded from the caller.
    unsafe {
        let state = value(description, c"Power Source State");
        Some(PowerSource {
            kind: string(value(description, c"Type")).unwrap_or_default(),
            name: string(value(description, c"Name")),
            id: number(description, c"Power Source ID"),
            present: number(description, c"Is Present").is_none_or(|present| present != 0),
            current: number(description, c"Current Capacity"),
            max: number(description, c"Max Capacity"),
            charging: number(description, c"Is Charging").map(|flag| flag != 0),
            charged: number(description, c"Is Charged").map(|flag| flag != 0),
            on_ac: (!state.is_null()).then(|| is_string(state, c"AC Power")),
        })
    }
}

/// A Bluetooth HID accessory (Magic Mouse, Keyboard, Trackpad, …) with
/// a battery reading.
#[derive(Debug, Clone)]
pub(crate) struct HidBattery {
    /// `IORegistryEntryGetRegistryEntryID`, stable while connected.
    pub(crate) registry_id: u64,
    /// `Product`.
    pub(crate) product: Option<String>,
    /// `BatteryPercent`, `0..=100`.
    pub(crate) percent: i64,
}

/// Every `AppleDeviceManagementHIDEventService` that reports a
/// `BatteryPercent`.
pub(crate) fn hid_batteries() -> Vec<HidBattery> {
    objc2::rc::autoreleasepool(|_| {
        let mut out = Vec::new();
        // SAFETY: the matching dictionary is consumed by
        // `IOServiceGetMatchingServices`; the iterator and every entry
        // it yields are released below.
        unsafe {
            let matching = IOServiceMatching(c"AppleDeviceManagementHIDEventService".as_ptr());
            if matching.is_null() {
                return out;
            }
            let mut iterator = 0_u32;
            if IOServiceGetMatchingServices(MAIN_PORT_DEFAULT, matching, &raw mut iterator)
                != KERN_SUCCESS
            {
                return out;
            }
            loop {
                let entry = IOIteratorNext(iterator);
                if entry == 0 {
                    break;
                }
                if let Some(battery) = hid_battery(entry) {
                    out.push(battery);
                }
                IOObjectRelease(entry);
            }
            IOObjectRelease(iterator);
        }
        out
    })
}

/// # Safety
///
/// `entry` is a live registry entry; inside a pool.
unsafe fn hid_battery(entry: u32) -> Option<HidBattery> {
    // SAFETY: forwarded from the caller; each property is a +1 CF
    // object released once read.
    unsafe {
        let percent = registry_property(entry, c"BatteryPercent", |object| number_value(object))?;
        let product = registry_property(entry, c"Product", |object| string(object));
        let mut registry_id = 0_u64;
        IORegistryEntryGetRegistryEntryID(entry, &raw mut registry_id);
        Some(HidBattery {
            registry_id,
            product,
            percent,
        })
    }
}

/// Read one registry property through `read`, releasing the copy.
///
/// # Safety
///
/// `entry` is a live registry entry; inside a pool.
unsafe fn registry_property<T>(
    entry: u32,
    key: &CStr,
    read: impl FnOnce(*const AnyObject) -> Option<T>,
) -> Option<T> {
    // SAFETY: forwarded from the caller.
    unsafe {
        let key = ns_string(key);
        if key.is_null() {
            return None;
        }
        let property = IORegistryEntryCreateCFProperty(entry, key.cast(), std::ptr::null(), 0);
        if property.is_null() {
            return None;
        }
        let read = read(property.cast());
        CFRelease(property);
        read
    }
}

/// An autoreleased `NSString` copy of `text`, or NULL.
///
/// # Safety
///
/// Call inside an autorelease pool.
pub(crate) unsafe fn ns_string(text: &CStr) -> *const AnyObject {
    let Some(class) = AnyClass::get(c"NSString") else {
        return std::ptr::null();
    };
    // SAFETY: `text` is NUL terminated; the result is autoreleased.
    let string: *const AnyObject = unsafe { msg_send![class, stringWithUTF8String: text.as_ptr()] };
    string
}

/// `dictionary[key]`, or NULL.
///
/// # Safety
///
/// `dictionary` is a live `NSDictionary`; inside a pool.
pub(crate) unsafe fn value(dictionary: *const AnyObject, key: &CStr) -> *const AnyObject {
    // SAFETY: forwarded from the caller.
    unsafe {
        let key = ns_string(key);
        if key.is_null() {
            return std::ptr::null();
        }
        let found: *const AnyObject = msg_send![dictionary, objectForKey: key];
        found
    }
}

/// `dictionary[key]` as an integer, when it is an `NSNumber`
/// (`CFNumber` and `CFBoolean` both bridge to it).
///
/// # Safety
///
/// As [`value`].
pub(crate) unsafe fn number(dictionary: *const AnyObject, key: &CStr) -> Option<i64> {
    // SAFETY: forwarded from the caller.
    unsafe { number_value(value(dictionary, key)) }
}

/// `object` as an integer, when it is an `NSNumber`.
///
/// # Safety
///
/// `object` is NULL or a live object.
pub(crate) unsafe fn number_value(object: *const AnyObject) -> Option<i64> {
    if object.is_null() {
        return None;
    }
    let class = AnyClass::get(c"NSNumber")?;
    // SAFETY: the receiver is live.
    unsafe {
        let is_number: bool = msg_send![object, isKindOfClass: class];
        is_number.then(|| msg_send![object, longLongValue])
    }
}

/// `object` as a Rust string, when it is an `NSString`.
///
/// # Safety
///
/// `object` is NULL or a live object.
pub(crate) unsafe fn string(object: *const AnyObject) -> Option<String> {
    if object.is_null() {
        return None;
    }
    let class = AnyClass::get(c"NSString")?;
    // SAFETY: the receiver is live; `UTF8String` points into storage
    // owned by the string (or the current pool), copied right away.
    unsafe {
        let is_string: bool = msg_send![object, isKindOfClass: class];
        if !is_string {
            return None;
        }
        let bytes: *const c_char = msg_send![object, UTF8String];
        (!bytes.is_null()).then(|| CStr::from_ptr(bytes).to_string_lossy().into_owned())
    }
}

/// Whether `object` is an `NSString` equal to `expected`.
///
/// # Safety
///
/// `object` is NULL or a live object; inside a pool.
pub(crate) unsafe fn is_string(object: *const AnyObject, expected: &CStr) -> bool {
    // SAFETY: forwarded from the caller.
    unsafe { string(object) }.is_some_and(|text| text.as_bytes() == expected.to_bytes())
}
