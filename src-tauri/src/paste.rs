//! Focus capture + auto-paste (macOS only), pure CoreFoundation/CoreGraphics.
//!
//! When the bar is triggered we record which app owns the frontmost normal
//! window (the one the user was typing in). After transcription we deliver a
//! synthetic Cmd+V *directly to that process* with `CGEventPostToPid`, so the
//! text lands in the field they left — no window activation, no focus theft.
//! The clipboard is already populated by `stop_recording`, so the Copy button
//! keeps working too (this is the "Both" behaviour).
//!
//! Written as raw C-ABI FFI on purpose: these framework symbols are stable, so
//! it compiles without pulling version-sensitive wrapper crates.

use std::ffi::c_void;

type CFTypeRef = *const c_void;
type CFArrayRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFStringRef = *const c_void;
type CFNumberRef = *const c_void;
type CGEventSourceRef = *const c_void;
type CGEventRef = *const c_void;

// CGWindowList options
const K_CG_WINDOW_LIST_ON_SCREEN_ONLY: u32 = 1 << 0;
const K_CG_WINDOW_LIST_EXCLUDE_DESKTOP: u32 = 1 << 4;
const K_CG_NULL_WINDOW_ID: u32 = 0;

// CFNumber type for a 32-bit signed int
const K_CF_NUMBER_SINT32: isize = 3;

// keyboard event synthesis
const K_CG_EVENT_SOURCE_COMBINED_SESSION_STATE: u32 = 0;
const KEYCODE_V: u16 = 0x09;
const FLAG_MASK_COMMAND: u64 = 0x0010_0000; // kCGEventFlagMaskCommand

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> CFArrayRef;
    fn CGEventSourceCreate(state_id: u32) -> CGEventSourceRef;
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        keycode: u16,
        keydown: bool,
    ) -> CGEventRef;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventPostToPid(pid: i32, event: CGEventRef);

    // CFString keys exported by CoreGraphics for the window-info dictionaries.
    static kCGWindowOwnerPID: CFStringRef;
    static kCGWindowLayer: CFStringRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFArrayGetCount(array: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, index: isize) -> *const c_void;
    fn CFDictionaryGetValue(dict: CFDictionaryRef, key: *const c_void) -> *const c_void;
    // Returns CoreFoundation `Boolean` (unsigned char), not C _Bool.
    fn CFNumberGetValue(number: CFNumberRef, the_type: isize, value_ptr: *mut c_void) -> u8;
    fn CFRelease(cf: CFTypeRef);
}

// Read an i32 out of a CFNumber-valued dictionary entry.
unsafe fn dict_i32(dict: CFDictionaryRef, key: CFStringRef) -> Option<i32> {
    let value = CFDictionaryGetValue(dict, key);
    if value.is_null() {
        return None;
    }
    let mut out: i32 = 0;
    let ok = CFNumberGetValue(
        value as CFNumberRef,
        K_CF_NUMBER_SINT32,
        &mut out as *mut i32 as *mut c_void,
    );
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// PID of the app owning the frontmost *normal* window (layer 0). Call this the
/// instant the bar is triggered. Our own bar floats above layer 0, so it's
/// skipped automatically; `fire_trigger` also guards against our own PID.
pub fn frontmost_pid() -> Option<i32> {
    unsafe {
        let list = CGWindowListCopyWindowInfo(
            K_CG_WINDOW_LIST_ON_SCREEN_ONLY | K_CG_WINDOW_LIST_EXCLUDE_DESKTOP,
            K_CG_NULL_WINDOW_ID,
        );
        if list.is_null() {
            return None;
        }

        let mut found: Option<i32> = None;
        let count = CFArrayGetCount(list);
        // The array is front-to-back, so the first layer-0 window wins.
        for i in 0..count {
            let dict = CFArrayGetValueAtIndex(list, i) as CFDictionaryRef;
            if dict.is_null() {
                continue;
            }
            let layer = dict_i32(dict, kCGWindowLayer).unwrap_or(-1);
            if layer != 0 {
                continue;
            }
            if let Some(pid) = dict_i32(dict, kCGWindowOwnerPID) {
                found = Some(pid);
                break;
            }
        }

        CFRelease(list);
        found
    }
}

/// Deliver Cmd+V straight to `pid` — pastes the clipboard into that app's key
/// field without activating it or moving focus.
pub fn paste_into(pid: i32) {
    unsafe {
        let source = CGEventSourceCreate(K_CG_EVENT_SOURCE_COMBINED_SESSION_STATE);

        let key_down = CGEventCreateKeyboardEvent(source, KEYCODE_V, true);
        CGEventSetFlags(key_down, FLAG_MASK_COMMAND);
        CGEventPostToPid(pid, key_down);

        let key_up = CGEventCreateKeyboardEvent(source, KEYCODE_V, false);
        CGEventSetFlags(key_up, FLAG_MASK_COMMAND);
        CGEventPostToPid(pid, key_up);

        if !key_down.is_null() {
            CFRelease(key_down);
        }
        if !key_up.is_null() {
            CFRelease(key_up);
        }
        if !source.is_null() {
            CFRelease(source);
        }
    }
}
