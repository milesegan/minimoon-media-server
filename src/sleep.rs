//! Keeping the host awake while it's serving (spec §8, "Sleep").
//!
//! While any content response is streaming, and for a grace period after the last one ends, the
//! server holds a "prevent idle sleep" assertion: IOKit's `PreventUserIdleSystemSleep` on macOS,
//! `SetThreadExecutionState` on Windows. Players buffer ahead and go quiet mid-playback, so the
//! grace period bridges the gaps. "Keep this computer awake while sharing" holds it always.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often the assertion is reconsidered.
const POLL: Duration = Duration::from_secs(5);

/// What the sleep thread watches.
pub struct Activity {
    streaming: AtomicUsize,
    last_end: Mutex<Option<Instant>>,
    keep_awake: AtomicBool,
    grace: Duration,
}

impl Activity {
    pub fn new(grace: Duration) -> Self {
        Self {
            streaming: AtomicUsize::new(0),
            last_end: Mutex::new(None),
            keep_awake: AtomicBool::new(false),
            grace,
        }
    }

    pub fn started(&self) {
        self.streaming.fetch_add(1, Ordering::SeqCst);
    }

    pub fn finished(&self) {
        self.streaming.fetch_sub(1, Ordering::SeqCst);
        *self.last_end.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }

    pub fn set_keep_awake(&self, on: bool) {
        self.keep_awake.store(on, Ordering::SeqCst);
    }

    /// Whether the host should stay awake now.
    pub fn wants_awake(&self) -> bool {
        self.keep_awake.load(Ordering::SeqCst)
            || self.streaming.load(Ordering::SeqCst) > 0
            || self
                .last_end
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some_and(|at| at.elapsed() < self.grace)
    }
}

/// Runs the sleep thread until `stop` is set.
pub fn spawn(activity: Arc<Activity>, stop: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new()
        .name("mmp-sleep".into())
        .spawn(move || {
            let mut assertion = platform::Assertion::default();
            while !stop.load(Ordering::SeqCst) {
                assertion.set(activity.wants_awake());
                std::thread::sleep(POLL);
            }
            assertion.set(false);
        });
}

#[cfg(target_os = "macos")]
mod platform {
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            level: u32,
            name: CFStringRef,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    const LEVEL_ON: u32 = 255;

    #[derive(Default)]
    pub struct Assertion(Option<u32>);

    impl Assertion {
        pub fn set(&mut self, on: bool) {
            match (on, self.0) {
                (true, None) => {
                    let kind = CFString::new("PreventUserIdleSystemSleep");
                    let name = CFString::new("MiniMoon Media Server is sharing files");
                    let mut id = 0u32;
                    // SAFETY: valid CFStrings and an out pointer to a local.
                    let status = unsafe {
                        IOPMAssertionCreateWithName(
                            kind.as_concrete_TypeRef(),
                            LEVEL_ON,
                            name.as_concrete_TypeRef(),
                            &mut id,
                        )
                    };
                    if status == 0 {
                        tracing::debug!("holding the idle-sleep assertion");
                        self.0 = Some(id);
                    }
                }
                (false, Some(id)) => {
                    // SAFETY: id came from IOPMAssertionCreateWithName.
                    unsafe { IOPMAssertionRelease(id) };
                    tracing::debug!("released the idle-sleep assertion");
                    self.0 = None;
                }
                _ => {}
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
    };

    #[derive(Default)]
    pub struct Assertion(bool);

    impl Assertion {
        /// Called only from the sleep thread, which the execution state belongs to.
        pub fn set(&mut self, on: bool) {
            if on != self.0 {
                let flags = if on {
                    ES_CONTINUOUS | ES_SYSTEM_REQUIRED
                } else {
                    ES_CONTINUOUS
                };
                // SAFETY: a plain call with valid flags.
                unsafe { SetThreadExecutionState(flags) };
                self.0 = on;
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    #[derive(Default)]
    pub struct Assertion;

    impl Assertion {
        pub fn set(&mut self, _on: bool) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_awake_through_the_grace_period() {
        let activity = Activity::new(Duration::from_millis(100));
        assert!(!activity.wants_awake());
        activity.started();
        assert!(activity.wants_awake());
        activity.finished();
        assert!(activity.wants_awake());
        std::thread::sleep(Duration::from_millis(150));
        assert!(!activity.wants_awake());
        activity.set_keep_awake(true);
        assert!(activity.wants_awake());
    }
}
