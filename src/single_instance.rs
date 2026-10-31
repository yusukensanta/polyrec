//! One PolyRec at a time per copy of the program. Launching it again while
//! it runs -- easy to do once it's hidden in the tray -- brings the running
//! one up instead of starting a second, which would register every hotkey
//! again, put up a second tray icon and save over the same config.toml.
//! Each copy (by exe path) counts on its own, so a portable copy and an
//! installed one don't stand in each other's way, and neither do
//! development builds.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{HANDLE, WAIT_ABANDONED, WAIT_EVENT, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, INFINITE, ReleaseMutex, SetEvent, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
use windows::core::HSTRING;

/// The command-line flag a self-update's relaunch carries: the copy it
/// replaces is still closing, so wait for that instead of bringing it up.
pub const AFTER_UPDATE_ARG: &str = "--after-update";

/// How long a relaunch after an update waits for the copy it replaces to
/// exit -- which may first have a recording to finish saving.
const PREVIOUS_EXIT_TIMEOUT_MS: u32 = 60_000;

/// This process is the running copy. Its claim lasts until `release`, or
/// until the process exits: the handles are never closed, and the claim
/// belongs to the thread that called `claim` -- the main thread, which runs
/// until then.
pub struct Instance {
    /// Signalled by a later launch -- see `listen`. `None` if Windows
    /// wouldn't make it (logged).
    show_event: Option<usize>,
    /// The claim itself; `None` if Windows wouldn't make the mutex (logged),
    /// in which case there's nothing to release.
    mutex: Option<usize>,
    /// Set by `release`.
    released: Arc<AtomicBool>,
}

/// Claims the running copy for this process. If another process has it,
/// asks that one to show its window and returns `None`; with
/// `after_update` (see `AFTER_UPDATE_ARG`), waits for it to exit instead.
/// Call it on the main thread.
pub fn claim(after_update: bool) -> Option<Instance> {
    let timeout = if after_update {
        PREVIOUS_EXIT_TIMEOUT_MS
    } else {
        0
    };
    claim_named(&instance_name(), timeout)
}

fn claim_named(name: &str, timeout_ms: u32) -> Option<Instance> {
    unsafe {
        // Made (or opened) before claiming, so a request made while the
        // running copy is still starting up waits for it to listen.
        let show_event = CreateEventW(None, false, false, &HSTRING::from(format!("{name}-show")))
            .inspect_err(|e| tracing::warn!("a later launch can't bring this one up: {e}"))
            .ok();
        let mut instance = Instance {
            show_event: show_event.map(|event| event.0 as usize),
            mutex: None,
            released: Arc::new(AtomicBool::new(false)),
        };
        let mutex = match CreateMutexW(None, false, &HSTRING::from(name)) {
            Ok(mutex) => mutex,
            Err(e) => {
                tracing::warn!("can't tell whether PolyRec is already running: {e}");
                return Some(instance);
            }
        };
        if owned(WaitForSingleObject(mutex, timeout_ms)) {
            instance.mutex = Some(mutex.0 as usize);
            return Some(instance);
        }
        // The user just started this process, so it may hand the right to
        // come to the front over to the running copy.
        if let Some(event) = show_event {
            let _ = AllowSetForegroundWindow(ASFW_ANY);
            let _ = SetEvent(event);
        }
        None
    }
}

/// Whether a wait on a mutex got it -- including from a copy that exited
/// without letting go (abandoned).
fn owned(wait: WAIT_EVENT) -> bool {
    wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED
}

impl Instance {
    /// Calls `on_show`, on a thread of its own, every time a later launch
    /// asks this copy to show itself -- until `release`.
    pub fn listen(&self, on_show: impl Fn() + Send + 'static) {
        let Some(event) = self.show_event else {
            return;
        };
        let released = Arc::clone(&self.released);
        let spawned = std::thread::Builder::new()
            .name("ShowRequests".into())
            .spawn(move || {
                let event = HANDLE(event as *mut core::ffi::c_void);
                while unsafe { WaitForSingleObject(event, INFINITE) } == WAIT_OBJECT_0 {
                    if released.load(Ordering::SeqCst) {
                        // Meant for the copy that's up now: pass it back.
                        let _ = unsafe { SetEvent(event) };
                        return;
                    }
                    on_show();
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("a later launch can't bring this one up: {e}");
        }
    }
}

impl Instance {
    /// Lets a later launch run as the copy that's up, once this one's window
    /// is gone but the process isn't yet -- it may still be finishing a
    /// recording's save. Call it on the thread that called `claim`.
    pub fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        if let Some(mutex) = self.mutex {
            let mutex = HANDLE(mutex as *mut core::ffi::c_void);
            if let Err(e) = unsafe { ReleaseMutex(mutex) } {
                tracing::warn!("couldn't let a later launch run yet: {e}");
            }
        }
    }
}

/// `Local\PolyRec-<hash of this exe's path>`: one per copy of the program,
/// per Windows session.
fn instance_name() -> String {
    let path = std::env::current_exe()
        .map(|path| path.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    format!("Local\\PolyRec-{:016x}", fnv1a(path.as_bytes()))
}

/// FNV-1a. Unlike `std`'s hasher, it stays the same from one build to the
/// next, which a relaunch after an update relies on to find the copy it
/// replaces.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn fnv1a_matches_its_published_test_vectors() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn the_name_is_this_copys_own_and_stays_the_same() {
        let name = instance_name();
        assert!(name.starts_with("Local\\PolyRec-"), "{name}");
        assert_eq!(name.len(), "Local\\PolyRec-".len() + 16);
        assert_eq!(name, instance_name());
    }

    /// A unique name per test run, so tests don't see each other's (or a
    /// real PolyRec's) claims.
    fn test_name(test: &str) -> String {
        format!("Local\\PolyRec-test-{test}-{}", std::process::id())
    }

    /// Claims `name` on a thread of its own, which holds the claim until
    /// told to let go: sending `true` on the returned sender releases it
    /// (`Instance::release`) and keeps the thread running, as a closing
    /// PolyRec finishing a save does; sending `false`, or dropping the
    /// sender, ends the thread. Returns once the claim was tried, with
    /// whether it got it.
    fn claim_on_a_thread(
        name: &str,
        timeout_ms: u32,
        shows: Arc<AtomicU32>,
    ) -> (bool, mpsc::Sender<bool>, std::thread::JoinHandle<()>) {
        let (claimed_tx, claimed_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<bool>();
        let name = name.to_owned();
        let thread = std::thread::spawn(move || {
            let instance = claim_named(&name, timeout_ms);
            if let Some(instance) = &instance {
                instance.listen(move || {
                    shows.fetch_add(1, Ordering::SeqCst);
                });
            }
            claimed_tx.send(instance.is_some()).unwrap();
            while let Ok(true) = release_rx.recv() {
                if let Some(instance) = &instance {
                    instance.release();
                }
            }
        });
        (claimed_rx.recv().unwrap(), release_tx, thread)
    }

    fn wait_for(shows: &AtomicU32, count: u32) {
        for _ in 0..200 {
            if shows.load(Ordering::SeqCst) >= count {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "got {} show requests, not {count}",
            shows.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn a_second_launch_brings_the_first_up_instead_of_running() {
        let name = test_name("second-launch");
        let shows = Arc::new(AtomicU32::new(0));
        let (first, release_first, first_thread) = claim_on_a_thread(&name, 0, Arc::clone(&shows));
        assert!(first);

        let (second, _release, second_thread) = claim_on_a_thread(&name, 0, Arc::new(0.into()));
        assert!(!second, "a second copy claimed it too");
        wait_for(&shows, 1);
        let (third, _release, third_thread) = claim_on_a_thread(&name, 0, Arc::new(0.into()));
        assert!(!third);
        wait_for(&shows, 2);

        drop(release_first);
        first_thread.join().unwrap();
        drop((second_thread, third_thread));
    }

    #[test]
    fn a_launch_while_the_last_copy_finishes_saving_runs_and_gets_show_requests() {
        let name = test_name("released");
        let old_shows = Arc::new(AtomicU32::new(0));
        let (old, release_old, _old_thread) = claim_on_a_thread(&name, 0, Arc::clone(&old_shows));
        assert!(old);
        // The old copy's window is gone; its process lives on, saving.
        release_old.send(true).unwrap();

        let new_shows = Arc::new(AtomicU32::new(0));
        let (new, _release_new, _new_thread) = claim_on_a_thread(&name, 0, Arc::clone(&new_shows));
        assert!(new, "a launch after the window closed didn't run");

        // A third launch asks the copy that's up to show itself: that's the
        // new one, even if the old one's listener wakes up first.
        let (third, _release, _thread) = claim_on_a_thread(&name, 0, Arc::new(0.into()));
        assert!(!third);
        wait_for(&new_shows, 1);
        assert_eq!(old_shows.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_relaunch_after_an_update_waits_for_the_copy_it_replaces() {
        let name = test_name("after-update");
        let shows = Arc::new(AtomicU32::new(0));
        let (first, release_first, first_thread) = claim_on_a_thread(&name, 0, Arc::clone(&shows));
        assert!(first);

        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(release_first);
            first_thread.join().unwrap();
        });
        let (relaunched, _release, _thread) = claim_on_a_thread(&name, 5_000, Arc::new(0.into()));
        assert!(
            relaunched,
            "the relaunch didn't get it once the old copy let go"
        );
        assert_eq!(
            shows.load(Ordering::SeqCst),
            0,
            "the old copy was asked to show"
        );
        releaser.join().unwrap();
    }
}
