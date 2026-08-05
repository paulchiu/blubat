//! Device arrivals and departures, as a nudge a poll tier can wait on.
//!
//! A connect or a disconnect is the moment a cached reading is most
//! misleading, so IOKit's own matched and terminated notifications wake the
//! poller instead of leaving it to find out on its next tick. Nothing is read
//! here: a nudge only says that reading again is worth doing now.
//!
//! What it covers is what [`crate::iokit`] covers, the Apple HID class, since
//! that is the class whose registry entries come and go with the link. A
//! device that reports through `system_profiler` alone still arrives on the
//! ordinary tick.

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::mpsc::Sender;
use std::thread;

use objc2_core_foundation::{CFDictionary, CFRetained, CFRunLoop, kCFRunLoopDefaultMode};
use objc2_io_kit::{
    IOIteratorNext, IONotificationPort, IONotificationPortRef, IOObjectRelease,
    IOServiceAddMatchingNotification, IOServiceMatching, io_iterator_t, kIOMainPortDefault,
    kIOMatchedNotification, kIOTerminatedNotification,
};

use crate::iokit::SERVICE_CLASS;

/// Starts watching, sending each change onto `nudged`.
///
/// The caller owns the channel, since a poll tier's own manual refresh sends
/// on the very same one: to either side, a person asking and IOKit reporting
/// are the same nudge. The watcher runs a blocked run loop on a thread of its
/// own, and ends itself on the first change after the poller has gone away.
/// Where IOKit refuses a notification port `nudged` is simply never fed,
/// which leaves the poller on its plain intervals rather than failing it.
pub(crate) fn watch(nudged: Sender<()>) {
    thread::spawn(move || listen(&nudged));
}

/// Arms both notifications and runs the loop that delivers them.
///
/// `nudged` outlives every callback because the run loop returns before this
/// function does, which is what makes handing IOKit a pointer to it sound.
fn listen(nudged: &Sender<()>) {
    let port = IONotificationPort::create(unsafe { kIOMainPortDefault });
    if port.is_null() {
        return;
    }

    let refcon: *mut c_void = std::ptr::from_ref(nudged).cast_mut().cast();
    let armed = [kIOMatchedNotification, kIOTerminatedNotification]
        .map(|kind| register(port, kind, refcon));

    if let Some(source) = unsafe { IONotificationPort::run_loop_source(port) }
        && let Some(run_loop) = CFRunLoop::current()
    {
        run_loop.add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });
        CFRunLoop::run();
    }

    for iterator in armed.into_iter().flatten() {
        IOObjectRelease(iterator);
    }
    unsafe { IONotificationPort::destroy(port) };
}

/// Arms one notification, yielding the iterator its caller has to release.
///
/// Each `unsafe` wraps the single call it vouches for, as [`crate::iokit`]
/// already models, so the rest reads as the plainly safe code it is.
fn register(
    port: IONotificationPortRef,
    kind: &CStr,
    refcon: *mut c_void,
) -> Option<io_iterator_t> {
    let class = CString::new(SERVICE_CLASS).expect("class name has no interior nul");
    let matching = unsafe { IOServiceMatching(class.as_ptr()) }?;
    // IOServiceMatching hands back the mutable subtype and this call wants the
    // immutable one, of which it consumes the reference. Same object, so the
    // reinterpret is sound.
    let matching: CFRetained<CFDictionary> = unsafe { CFRetained::cast_unchecked(matching) };

    let mut iterator: io_iterator_t = 0;
    let result = unsafe {
        IOServiceAddMatchingNotification(
            port,
            // The C parameter is a `char[128]`, which a notification name is
            // read out of as the plain string it already is.
            kind.as_ptr().cast_mut().cast::<[c_char; 128]>(),
            Some(matching),
            Some(changed),
            refcon,
            &mut iterator,
        )
    };
    if result != 0 || iterator == 0 {
        return None;
    }

    empty(iterator);

    Some(iterator)
}

/// Delivers one nudge, and ends the run loop once nobody is listening.
///
/// # Safety
///
/// Called by IOKit with the `refcon` [`listen`] registered, which is a pointer
/// to a sender that outlives the run loop.
unsafe extern "C-unwind" fn changed(refcon: *mut c_void, iterator: io_iterator_t) {
    empty(iterator);

    let nudged = unsafe { &*refcon.cast::<Sender<()>>() };
    if nudged.send(()).is_err()
        && let Some(run_loop) = CFRunLoop::current()
    {
        run_loop.stop();
    }
}

/// Empties an iterator, releasing every handle it hands out.
///
/// Both the arming and the re-arming of a notification: IOKit arms it only
/// once the iterator it came with has run dry. What was in it is not read
/// here, since the poll that follows reads every device anyway.
fn empty(iterator: io_iterator_t) {
    loop {
        let entry = IOIteratorNext(iterator);
        if entry == 0 {
            break;
        }

        IOObjectRelease(entry);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::Duration;

    use super::*;

    /// Ignored by default: it registers against the real IOKit registry and
    /// waits for a hand to unplug something. Run it with
    /// `cargo test -- --ignored presence` and reconnect a Magic Trackpad.
    #[test]
    #[ignore = "needs a real machine and a device to connect or disconnect"]
    fn a_real_device_change_arrives_as_a_nudge() {
        let (nudged, nudges) = mpsc::channel();
        watch(nudged);

        assert_eq!(
            nudges.recv_timeout(Duration::from_secs(30)),
            Ok(()),
            "no notification arrived: connect or disconnect a Magic peripheral"
        );
        assert_eq!(
            nudges.recv_timeout(Duration::from_millis(100)),
            Err(RecvTimeoutError::Timeout),
            "one change is one nudge, not a stream of them"
        );
    }
}
