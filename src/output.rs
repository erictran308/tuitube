//! Pausing when the sound's device goes, as an iPhone does (macOS): with
//! `pause_on_unplug`, headphones pulled out, or a Bluetooth headset or
//! speaker turned off or out of reach, pause what plays rather than let it
//! go on from the speakers. Picking another output yourself doesn't.
//!
//! mpv can't say so. With no `audio-device` it plays to the system's default
//! output (CoreAudio's DefaultOutput unit), follows it to the next one and
//! never pauses, and over its socket only `audio-device-list` changes: for
//! any device that comes or goes, used or not, and not for one picked. So
//! tuitube asks CoreAudio itself, as mpv does: [`watch`] starts a thread of
//! tuitube's that CoreAudio wakes whenever the devices or the default output
//! change, and that reads them ([`went_away`]). Measured (macOS 27, an
//! aggregate device standing in for headphones, the ignored `unplugging_`
//! test below): when the default output goes, the devices change first, the
//! default still naming the one gone, then the default, 5 ms later; one
//! picked changes only the default, the old one still listed. The other
//! order counts too ([`GOING_WITHIN`]): not every kind of device was tried.
//! Macs since 2018 (T2, Apple Silicon) list their headphone jack as a device
//! of its own, "External Headphones", only while something is plugged in;
//! on older ones it's a source of the built-in output, not a device, and
//! pulling it isn't seen. The computer's own speakers going is never an
//! unplug ([`Device::speakers`]).
//!
//! Only device numbers are read; nothing is sent anywhere. Watched from the
//! first video played with the setting on (`App::watch_output`), for the
//! rest of the run.

use std::time::{Duration, SystemTime};

use tokio::sync::mpsc::UnboundedSender;

use crate::app::AppEvent;

/// tuitube sees the sound's device go: macOS.
pub const AVAILABLE: bool = cfg!(target_os = "macos");

/// No device: CoreAudio's `kAudioObjectUnknown`.
const NONE: u32 = 0;

/// How soon after the sound moves off a device, that device going counts
/// as the one playing going: CoreAudio may say the default output changed
/// before it says the device went, and the two are read one after the
/// other. A device picked away from and pulled out this soon after pauses
/// too. Wall-clock time, which goes on while the Mac sleeps.
const GOING_WITHIN: Duration = Duration::from_secs(3);

/// Tells `tx` [`AppEvent::OutputGone`] each time the device playing the sound
/// goes, for the rest of the run, or [`AppEvent::OutputUnwatched`] if it
/// can't. Returns at once: CoreAudio is asked on the thread. Elsewhere than
/// macOS, nothing.
pub fn watch(tx: UnboundedSender<AppEvent>) {
    imp::watch(tx);
}

/// A device the sound played to, as it was when it did: once it's gone,
/// nothing more can be asked of it.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct Device {
    id: u32,
    /// The computer's own speakers. macOS may take them off the list while
    /// headphones are plugged in, so their going is never an unplug.
    speakers: bool,
    /// What the system calls it ("External Headphones"), as shown.
    name: String,
}

/// What the thread knows of the devices.
#[derive(Debug, Default)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct Seen {
    /// The default output while it's listed: the device playing the sound.
    playing: Device,
    /// The devices the sound moved off while they were still listed, and
    /// when: one going within [`GOING_WITHIN`] was the one playing.
    left: Vec<(Device, SystemTime)>,
}

/// The device playing the sound, if it went, from the devices listed now
/// and the default output now: its name. A device going is told once:
/// `playing` becomes the default output, or none while it isn't listed (the
/// default names a device that went until macOS picks another).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn went_away(seen: &mut Seen, listed: &[u32], default: Device, now: SystemTime) -> Option<String> {
    let gone =
        |device: &Device| device.id != NONE && !device.speakers && !listed.contains(&device.id);
    seen.left.retain(|(_, at)| {
        now.duration_since(*at)
            .is_ok_and(|since| since <= GOING_WITHIN)
    });
    let went = if gone(&seen.playing) {
        Some(seen.playing.name.clone())
    } else {
        let left = seen.left.iter().find(|(device, _)| gone(device));
        left.map(|(device, _)| device.name.clone())
    };
    if went.is_some() {
        seen.left.clear();
    } else if seen.playing.id != NONE && default.id != seen.playing.id {
        // Picked away from, or about to go.
        seen.left.push((seen.playing.clone(), now));
    }
    seen.playing = if listed.contains(&default.id) {
        default
    } else {
        Device::default()
    };
    went
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::ptr::null;
    use std::sync::mpsc::{Sender, channel};
    use std::time::SystemTime;

    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};
    use tokio::sync::mpsc::UnboundedSender;

    use super::{Device, NONE, Seen, went_away};
    use crate::app::AppEvent;

    /// The most of a device's name shown, in characters.
    const MAX_NAME: usize = 60;

    pub(super) type ObjectId = u32;
    pub(super) type Status = i32;

    /// CoreAudio's `AudioObjectPropertyAddress`.
    #[repr(C)]
    pub(super) struct Address {
        selector: u32,
        scope: u32,
        element: u32,
    }

    impl Address {
        /// A property of the whole object, as the system's are.
        pub(super) const fn global(selector: &[u8; 4]) -> Self {
            Self::scoped(selector, b"glob")
        }

        /// A property of a device's output.
        const fn output(selector: &[u8; 4]) -> Self {
            Self::scoped(selector, b"outp")
        }

        const fn scoped(selector: &[u8; 4], scope: &[u8; 4]) -> Self {
            Self {
                selector: u32::from_be_bytes(*selector),
                scope: u32::from_be_bytes(*scope),
                element: 0,
            }
        }
    }

    /// CoreAudio's `AudioObjectPropertyListenerProc`.
    type Listener = extern "C" fn(ObjectId, u32, *const Address, *mut c_void) -> Status;

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectGetPropertyDataSize(
            object: ObjectId,
            address: *const Address,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
        ) -> Status;
        pub(super) fn AudioObjectSetPropertyData(
            object: ObjectId,
            address: *const Address,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: u32,
            data: *const c_void,
        ) -> Status;
        pub(super) fn AudioObjectGetPropertyData(
            object: ObjectId,
            address: *const Address,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
            data: *mut c_void,
        ) -> Status;
        fn AudioObjectAddPropertyListener(
            object: ObjectId,
            address: *const Address,
            listener: Listener,
            data: *mut c_void,
        ) -> Status;
        fn AudioObjectRemovePropertyListener(
            object: ObjectId,
            address: *const Address,
            listener: Listener,
            data: *mut c_void,
        ) -> Status;
    }

    /// `kAudioObjectSystemObject`: the computer's sound as a whole.
    pub(super) const SYSTEM: ObjectId = 1;
    /// Every device, by number.
    const DEVICES: Address = Address::global(b"dev#");
    /// The device sound plays to unless a program picks one.
    pub(super) const DEFAULT_OUTPUT: Address = Address::global(b"dOut");
    /// The run loop the HAL tells on: none is a thread of its own.
    const RUN_LOOP: Address = Address::global(b"rnlp");

    pub fn watch(tx: UnboundedSender<AppEvent>) {
        let told = tx.clone();
        let started = std::thread::Builder::new()
            .name("output".into())
            .spawn(move || {
                if let Err(why) = follow(&tx) {
                    let _ = tx.send(AppEvent::OutputUnwatched(why));
                }
            });
        if let Err(e) = started {
            let _ = told.send(AppEvent::OutputUnwatched(e.to_string()));
        }
    }

    /// Has CoreAudio wake the thread on every change, and tells `tx` each
    /// device playing that went, until tuitube is gone.
    fn follow(tx: &UnboundedSender<AppEvent>) -> Result<(), String> {
        let (wake, woken) = channel::<()>();
        // Leaked: CoreAudio may call `changed` with it for the rest of the run
        // (and after a listener is taken off, it isn't said that it won't).
        let data = Box::into_raw(Box::new(wake)).cast::<c_void>();
        // Told on a thread of the HAL's own, not the main thread's run loop,
        // which tuitube never runs (macOS since 10.6 would use it, says the
        // deprecated property; 27 tells either way). Before anything else
        // asks the HAL, as it must be; if it can't, it's tried anyway.
        let none: *const c_void = null();
        // SAFETY: the property is a CFRunLoopRef, here none, `size` bytes.
        unsafe {
            AudioObjectSetPropertyData(
                SYSTEM,
                &RUN_LOOP,
                0,
                null(),
                size_of::<*const c_void>() as u32,
                (&raw const none).cast(),
            )
        };
        let addresses = [&DEVICES, &DEFAULT_OUTPUT];
        for (i, address) in addresses.iter().enumerate() {
            // SAFETY: `changed` has the listener's signature, and `data` is a
            // `Sender<()>` that's never freed.
            let status = unsafe { AudioObjectAddPropertyListener(SYSTEM, *address, changed, data) };
            if status != 0 {
                for address in &addresses[..i] {
                    // SAFETY: the listener added above, with the same `data`.
                    unsafe { AudioObjectRemovePropertyListener(SYSTEM, *address, changed, data) };
                }
                return Err(format!(
                    "CoreAudio doesn't say when the output changes (error {status})"
                ));
            }
        }
        let mut seen = Seen::default();
        look(&mut seen);
        while woken.recv().is_ok() {
            // One look for the wakes come meanwhile too: a device going wakes
            // it a few times, and the answer is the same.
            while woken.try_recv().is_ok() {}
            if let Some(name) = look(&mut seen)
                && tx.send(AppEvent::OutputGone(name)).is_err()
            {
                break;
            }
        }
        Ok(())
    }

    /// What CoreAudio calls, on a thread of its own, when the devices or the
    /// default output change: it only wakes tuitube's thread, so CoreAudio's
    /// never waits on tuitube.
    extern "C" fn changed(_: ObjectId, _: u32, _: *const Address, data: *mut c_void) -> Status {
        // SAFETY: the `Sender<()>` `follow` leaked, never freed. A `Sender` can
        // be sent through from any thread.
        let wake = unsafe { &*data.cast::<Sender<()>>() };
        let _ = wake.send(());
        0
    }

    /// The device playing, if it went, from what CoreAudio says now: its
    /// name. If CoreAudio can't say, nothing changes.
    fn look(seen: &mut Seen) -> Option<String> {
        let (Some(listed), Some(id)) = (ids(SYSTEM, &DEVICES), default_output()) else {
            return None;
        };
        // Asked of only when the sound moves to it.
        let default = if id == seen.playing.id {
            seen.playing.clone()
        } else {
            Device {
                id,
                speakers: speakers(id),
                name: name(id),
            }
        };
        went_away(seen, &listed, default, SystemTime::now())
    }

    /// Whether `id` is the computer's own speakers: built in (transport
    /// `bltn`), and a speaker: its data source `ispk`, or its stream's
    /// terminal a speaker, USB's 0x301 or CoreAudio's `spkr` (`ispk` and
    /// 0x301 measured on a MacBook Pro).
    fn speakers(id: ObjectId) -> bool {
        let code = |name: &[u8; 4]| u32::from_be_bytes(*name);
        let terminal = ids(id, &Address::output(b"stm#"))
            .and_then(|streams| number(*streams.first()?, &Address::global(b"term")));
        number(id, &Address::global(b"tran")) == Some(code(b"bltn"))
            && (number(id, &Address::output(b"ssrc")) == Some(code(b"ispk"))
                || terminal.is_some_and(|t| t == 0x301 || t == code(b"spkr")))
    }

    /// What the system calls the device, as it's shown; "Sound output" if
    /// it can't say. Its own text (a Bluetooth device names itself), so
    /// cleaned and cut short as a title is.
    fn name(id: ObjectId) -> String {
        let mut name: CFStringRef = null();
        let mut size = size_of::<CFStringRef>() as u32;
        // SAFETY: the name is a CFString, written to `name`.
        let status = unsafe {
            AudioObjectGetPropertyData(
                id,
                &Address::global(b"lnam"),
                0,
                null(),
                &mut size,
                (&raw mut name).cast(),
            )
        };
        if status != 0 || name.is_null() {
            return "Sound output".into();
        }
        // SAFETY: CoreAudio made it for the caller to release.
        let name = unsafe { CFString::wrap_under_create_rule(name) }.to_string();
        crate::video::one_line(&name, MAX_NAME)
    }

    /// A property that's a list of objects, if CoreAudio says it.
    fn ids(object: ObjectId, address: &Address) -> Option<Vec<ObjectId>> {
        let each = size_of::<ObjectId>();
        let mut size = 0u32;
        // SAFETY: CoreAudio writes the list's size, in bytes, to `size`.
        let status =
            unsafe { AudioObjectGetPropertyDataSize(object, address, 0, null(), &mut size) };
        if status != 0 {
            return None;
        }
        // Room for a few more, should one come between the two asks.
        let mut ids = vec![NONE; size as usize / each + 8];
        let mut size = (ids.len() * each) as u32;
        // SAFETY: `ids` holds `size` bytes; CoreAudio writes at most that,
        // and says how many it wrote.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                address,
                0,
                null(),
                &mut size,
                ids.as_mut_ptr().cast(),
            )
        };
        if status != 0 {
            return None;
        }
        ids.truncate(size as usize / each);
        Some(ids)
    }

    /// A property that's one number (or object), if CoreAudio says it.
    fn number(object: ObjectId, address: &Address) -> Option<u32> {
        let mut value = 0u32;
        let mut size = size_of::<u32>() as u32;
        // SAFETY: `value` is a u32, `size` bytes, for CoreAudio to write.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                address,
                0,
                null(),
                &mut size,
                (&raw mut value).cast(),
            )
        };
        (status == 0 && size == 4).then_some(value)
    }

    pub(super) fn default_output() -> Option<ObjectId> {
        number(SYSTEM, &DEFAULT_OUTPUT)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use tokio::sync::mpsc::UnboundedSender;

    use crate::app::AppEvent;

    /// Nothing yet: only macOS is watched.
    pub fn watch(_tx: UnboundedSender<AppEvent>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEAKERS: Device = Device {
        id: 127,
        speakers: true,
        name: String::new(),
    };
    const HEADPHONES: Device = Device {
        id: 196,
        speakers: false,
        name: String::new(),
    };
    const MONITOR: Device = Device {
        id: 134,
        speakers: false,
        name: String::new(),
    };

    fn went(seen: &mut Seen, listed: &[Device], default: Device, now: SystemTime) -> bool {
        let listed: Vec<u32> = listed.iter().map(|d| d.id).collect();
        went_away(seen, &listed, default, now).is_some()
    }

    #[test]
    fn the_device_that_went_is_told_by_its_name() {
        let now = SystemTime::now();
        let mut seen = Seen::default();
        let headphones = Device {
            name: "External Headphones".into(),
            ..HEADPHONES
        };
        went_away(&mut seen, &[SPEAKERS.id, headphones.id], headphones, now);
        let went = went_away(&mut seen, &[SPEAKERS.id], SPEAKERS, now);
        assert_eq!(went.as_deref(), Some("External Headphones"));
    }

    /// Plugged in, and macOS plays to them.
    fn listening(start: SystemTime) -> Seen {
        let mut seen = Seen::default();
        assert!(!went(&mut seen, &[SPEAKERS, MONITOR], SPEAKERS, start));
        assert_eq!(seen.playing, SPEAKERS, "at start");
        let all = [SPEAKERS, MONITOR, HEADPHONES];
        assert!(!went(&mut seen, &all, SPEAKERS, start));
        assert!(!went(&mut seen, &all, HEADPHONES, start));
        assert_eq!(seen.playing, HEADPHONES);
        seen
    }

    #[test]
    fn the_device_playing_going_is_told_once() {
        let now = SystemTime::now();
        let mut seen = listening(now);
        let left = [SPEAKERS, MONITOR];
        // Pulled out: the devices change first, the default still naming them.
        assert!(went(&mut seen, &left, HEADPHONES, now));
        assert_eq!(seen.playing, Device::default());
        assert!(!went(&mut seen, &left, HEADPHONES, now), "told once");
        // Then macOS picks the speakers.
        assert!(!went(&mut seen, &left, SPEAKERS, now));
        assert_eq!(seen.playing, SPEAKERS);
    }

    #[test]
    fn the_default_moving_first_is_told_too() {
        let now = SystemTime::now();
        let mut seen = listening(now);
        let all = [SPEAKERS, MONITOR, HEADPHONES];
        assert!(!went(&mut seen, &all, SPEAKERS, now), "not yet");
        let soon = now + Duration::from_millis(5);
        assert!(went(&mut seen, &[SPEAKERS, MONITOR], SPEAKERS, soon));
        assert!(
            !went(&mut seen, &[SPEAKERS, MONITOR], SPEAKERS, soon),
            "once"
        );
        // Both at once.
        let mut seen = listening(now);
        assert!(went(&mut seen, &[SPEAKERS], SPEAKERS, now));
        assert_eq!(seen.playing, SPEAKERS);
        assert!(!went(&mut seen, &[SPEAKERS], SPEAKERS, now));
    }

    #[test]
    fn the_sound_moving_twice_before_the_device_goes_is_told_too() {
        let now = SystemTime::now();
        let mut seen = listening(now);
        let all = [SPEAKERS, MONITOR, HEADPHONES];
        assert!(!went(&mut seen, &all, SPEAKERS, now));
        assert!(!went(&mut seen, &all, MONITOR, now));
        let soon = now + Duration::from_millis(5);
        assert!(went(&mut seen, &[SPEAKERS, MONITOR], MONITOR, soon));
    }

    #[test]
    fn another_device_picked_or_one_not_playing_going_isnt_told() {
        let now = SystemTime::now();
        let mut seen = listening(now);
        let all = [SPEAKERS, MONITOR, HEADPHONES];
        assert!(!went(&mut seen, &all, SPEAKERS, now), "picked");
        // Something else comes meanwhile: still watched.
        let more = Device {
            id: 5,
            speakers: false,
            name: String::new(),
        };
        assert!(!went(
            &mut seen,
            &[SPEAKERS, MONITOR, HEADPHONES, more],
            SPEAKERS,
            now
        ));
        assert!(!went(&mut seen, &all, SPEAKERS, now));
        // Pulled out a while after: the speakers played it by then.
        let later = now + GOING_WITHIN + Duration::from_millis(1);
        assert!(!went(&mut seen, &[SPEAKERS, MONITOR], SPEAKERS, later));
        assert!(
            !went(&mut seen, &[SPEAKERS], SPEAKERS, later),
            "not playing"
        );
    }

    #[test]
    fn the_speakers_going_as_headphones_come_isnt_told() {
        let now = SystemTime::now();
        let mut seen = Seen::default();
        assert!(!went(&mut seen, &[SPEAKERS, MONITOR], SPEAKERS, now));
        // Plugged in where macOS takes the speakers off the list: either way
        // round, nothing to pause.
        assert!(!went(&mut seen, &[MONITOR, HEADPHONES], SPEAKERS, now));
        assert!(!went(&mut seen, &[MONITOR, HEADPHONES], HEADPHONES, now));
        let mut other = Seen::default();
        assert!(!went(&mut other, &[SPEAKERS, MONITOR], SPEAKERS, now));
        assert!(!went(
            &mut other,
            &[SPEAKERS, MONITOR, HEADPHONES],
            HEADPHONES,
            now
        ));
        assert!(!went(&mut other, &[MONITOR, HEADPHONES], HEADPHONES, now));
        // Pulled out, the speakers back: that pauses.
        assert!(went(&mut seen, &[SPEAKERS, MONITOR], HEADPHONES, now));
    }

    /// Against the real CoreAudio: a device made to stand in for headphones
    /// (an aggregate of the default output) plays the sound, is picked away
    /// from and back, then goes. Changes the default output for a few
    /// seconds and puts it back, so it isn't one of the `live` tests that run
    /// together: `cargo test -- --ignored unplugging`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn unplugging_a_stand_in_for_headphones_is_told_once() {
        use tokio::sync::mpsc::UnboundedReceiver;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        watch(tx);
        // The watcher's thread asks the HAL first, as it must.
        std::thread::sleep(Duration::from_millis(500));
        let was = imp::default_output().expect("a default output");
        let _put_back = stand_in::PutBack(was);
        let headphones = stand_in::Aggregate::of(was);
        let settle = || std::thread::sleep(Duration::from_millis(1500));
        let told = |rx: &mut UnboundedReceiver<AppEvent>| {
            std::iter::from_fn(|| rx.try_recv().ok())
                .map(|e| match e {
                    AppEvent::OutputGone(name) => Ok(name),
                    AppEvent::OutputUnwatched(why) => Err(why),
                    _ => Err("something else".into()),
                })
                .collect::<Result<Vec<String>, String>>()
                .unwrap()
        };
        settle();
        for to in [headphones.id, was, headphones.id] {
            assert_eq!(stand_in::set_default(to), 0, "the default output set");
            settle();
        }
        assert_eq!(
            told(&mut rx),
            [""; 0],
            "a device coming, or picked, isn't told"
        );
        drop(headphones);
        settle();
        assert_eq!(
            told(&mut rx),
            ["tuitube test headphones"],
            "the one playing going is, once, by its name"
        );
    }
}

/// What the live test needs to make a device: an aggregate, described with
/// CoreFoundation's dictionaries.
#[cfg(all(test, target_os = "macos"))]
mod stand_in {
    use std::ptr::null;

    use core_foundation::array::CFArray;
    use core_foundation::base::TCFType;
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::number::CFNumber;
    use core_foundation::string::{CFString, CFStringRef};

    use super::imp::{
        Address, AudioObjectGetPropertyData, AudioObjectSetPropertyData, DEFAULT_OUTPUT, ObjectId,
        SYSTEM, Status, default_output,
    };

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioHardwareCreateAggregateDevice(
            description: CFDictionaryRef,
            device: *mut ObjectId,
        ) -> Status;
        fn AudioHardwareDestroyAggregateDevice(device: ObjectId) -> Status;
    }

    /// Makes `id` the default output; CoreAudio's answer, 0 if it did.
    pub fn set_default(id: ObjectId) -> Status {
        // SAFETY: `id` is an `ObjectId`, the property's type.
        unsafe {
            AudioObjectSetPropertyData(
                SYSTEM,
                &DEFAULT_OUTPUT,
                0,
                null(),
                size_of::<ObjectId>() as u32,
                (&raw const id).cast(),
            )
        }
    }

    /// Puts the default output back when dropped, the test passed or not.
    pub struct PutBack(pub ObjectId);

    impl Drop for PutBack {
        fn drop(&mut self) {
            if default_output() != Some(self.0) {
                set_default(self.0);
            }
        }
    }

    /// An output device of tuitube's own that plays to another, destroyed
    /// when dropped. Listed for every program, as it must be to be the
    /// default output: a run that's killed leaves it, in Audio MIDI Setup,
    /// as "tuitube test headphones".
    pub struct Aggregate {
        pub id: ObjectId,
    }

    impl Aggregate {
        pub fn of(device: ObjectId) -> Self {
            let mut uid: CFStringRef = null();
            let mut size = size_of::<CFStringRef>() as u32;
            // SAFETY: the device's UID is a CFString, written to `uid`.
            let status = unsafe {
                AudioObjectGetPropertyData(
                    device,
                    &Address::global(b"uid "),
                    0,
                    null(),
                    &mut size,
                    (&raw mut uid).cast(),
                )
            };
            assert!(status == 0 && !uid.is_null(), "the device's UID");
            // SAFETY: CoreAudio made it for the caller to release.
            let uid = unsafe { CFString::wrap_under_create_rule(uid) };
            let key = CFString::new;
            let sub = CFDictionary::from_CFType_pairs(&[(key("uid"), uid.as_CFType())]);
            let name = format!("io.github.erictran308.tuitube.test.{}", std::process::id());
            let description = CFDictionary::from_CFType_pairs(&[
                (key("uid"), CFString::new(&name).as_CFType()),
                (
                    key("name"),
                    CFString::new("tuitube test headphones").as_CFType(),
                ),
                (key("private"), CFNumber::from(0).as_CFType()),
                (key("subdevices"), CFArray::from_CFTypes(&[sub]).as_CFType()),
            ]);
            let mut id = super::NONE;
            // SAFETY: a description as CoreAudio documents it; `id` for the
            // new device's number.
            let status = unsafe {
                AudioHardwareCreateAggregateDevice(description.as_concrete_TypeRef(), &mut id)
            };
            assert_eq!(status, 0, "the aggregate device made");
            Self { id }
        }
    }

    impl Drop for Aggregate {
        fn drop(&mut self) {
            // SAFETY: the device made in `of`.
            unsafe { AudioHardwareDestroyAggregateDevice(self.id) };
        }
    }
}
