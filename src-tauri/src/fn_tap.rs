//! Global Fn/Globe double-tap detector (macOS only).
//!
//! Tauri's global-shortcut plugin sits on top of Carbon `RegisterEventHotKey`,
//! which never sees the Fn/Globe key — that key only shows up as a modifier-flag
//! change at the very bottom of the event stream. So to catch a *double-tap* of
//! Fn we install a `CGEventTap` on the session event stream, watch
//! `flagsChanged` events, and time the gap between two Fn key-down edges.
//!
//! This requires the app to be granted **Input Monitoring** (and/or
//! **Accessibility**) in System Settings → Privacy & Security. Until then
//! `CGEventTapCreate` returns null and we simply log and fall back to the
//! Ctrl+Shift+R shortcut.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ---- CoreFoundation / CoreGraphics FFI ----
// We hand-declare the C ABI instead of pulling a wrapper crate so this stays
// stable regardless of crate churn. These symbols live in the two system
// frameworks, which Tauri already links against.

type CFAllocatorRef = *const c_void;
type CFMachPortRef = *const c_void;
type CFRunLoopSourceRef = *const c_void;
type CFRunLoopRef = *const c_void;
type CFStringRef = *const c_void;
type CGEventTapProxy = *const c_void;
type CGEventRef = *const c_void;
type CGEventMask = u64;

// tap location / placement / options
const K_CG_SESSION_EVENT_TAP: u32 = 1;
const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const K_CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;

// event types
const K_CG_EVENT_FLAGS_CHANGED: u32 = 12;
const K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;

// The "Fn"/secondary-function modifier bit (NX_SECONDARYFNMASK).
const FLAG_SECONDARY_FN: u64 = 0x0080_0000;

// Two taps counted as a double-tap when the second press lands within this.
const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(450);

type CGEventTapCallBack = extern "C" fn(
    proxy: CGEventTapProxy,
    etype: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(
        alloc: CFAllocatorRef,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRun();
    static kCFRunLoopCommonModes: CFStringRef;
}

/// Everything the C callback needs, kept alive for the whole process.
struct TapContext {
    fn_down: AtomicBool,
    last_press: Mutex<Option<Instant>>,
    /// Stored as usize so we can re-enable the tap from the callback if macOS
    /// disables it after a slow frame.
    port: AtomicUsize,
    on_trigger: Box<dyn Fn() + Send + Sync>,
}

extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    etype: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    // SAFETY: user_info is the leaked TapContext pointer we passed to
    // CGEventTapCreate; it lives for the whole process.
    let ctx = unsafe { &*(user_info as *const TapContext) };

    // macOS occasionally disables a tap (e.g. it was too slow); re-arm it.
    if etype == K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT
        || etype == K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT
    {
        let port = ctx.port.load(Ordering::Relaxed) as CFMachPortRef;
        if !port.is_null() {
            unsafe { CGEventTapEnable(port, true) };
        }
        return event;
    }

    if etype == K_CG_EVENT_FLAGS_CHANGED {
        let flags = unsafe { CGEventGetFlags(event) };
        let fn_now = flags & FLAG_SECONDARY_FN != 0;
        let was_down = ctx.fn_down.swap(fn_now, Ordering::Relaxed);

        // Only react on the rising edge (key-down) of Fn.
        if fn_now && !was_down {
            let now = Instant::now();
            let mut last = ctx.last_press.lock().unwrap();
            let is_double = matches!(*last, Some(t) if now.duration_since(t) < DOUBLE_TAP_WINDOW);
            if is_double {
                *last = None;
                drop(last);
                (ctx.on_trigger)();
            } else {
                *last = Some(now);
            }
        }
    }

    // Listen-only tap: return the event untouched.
    event
}

/// Spawn the event-tap on its own thread with a dedicated run loop.
/// `on_trigger` fires on every Fn double-tap. Returns immediately.
pub fn spawn<F>(on_trigger: F)
where
    F: Fn() + Send + Sync + 'static,
{
    std::thread::spawn(move || {
        let ctx = Box::new(TapContext {
            fn_down: AtomicBool::new(false),
            last_press: Mutex::new(None),
            port: AtomicUsize::new(0),
            on_trigger: Box::new(on_trigger),
        });
        // Leak the context: it must outlive the callback for the app's lifetime.
        let ctx_ptr = Box::into_raw(ctx);

        let mask: CGEventMask = 1u64 << K_CG_EVENT_FLAGS_CHANGED;
        let port = unsafe {
            CGEventTapCreate(
                K_CG_SESSION_EVENT_TAP,
                K_CG_HEAD_INSERT_EVENT_TAP,
                K_CG_EVENT_TAP_OPTION_LISTEN_ONLY,
                mask,
                tap_callback,
                ctx_ptr as *mut c_void,
            )
        };

        if port.is_null() {
            eprintln!(
                "[fn_tap] CGEventTapCreate returned null — grant Input Monitoring/Accessibility \
                 to enable the Fn double-tap. Ctrl+Shift+R still works."
            );
            return;
        }

        // Record the port so the callback can re-enable the tap if needed.
        unsafe { &*ctx_ptr }.port.store(port as usize, Ordering::Relaxed);

        unsafe {
            let source = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
            CGEventTapEnable(port, true);
            // Blocks this thread forever, servicing the tap.
            CFRunLoopRun();
        }
    });
}
