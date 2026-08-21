//! The GATT sweep: a BLE peripheral's own Battery Service level, read over
//! CoreBluetooth and left in `readings.toml` beside the other two sweeps'
//! readings (see `blubat_core::gatt` for why a peripheral is matched by name,
//! and `blubat_core::readings` for the file the three share).
//!
//! Everything [`super::bmap`]'s module doc says about TCC and the main thread
//! holds here too, and for the same reason: CoreBluetooth delivers its
//! delegate callbacks through the run loop of whichever thread made the call,
//! so a sweep run anywhere but the daemon's own pumped main thread waits on a
//! run loop nothing ever turns. That is why [`sweep`] is reachable only from
//! [`super::sweep::execute`], which is that thread.
//!
//! No scanning, ever. `retrieveConnectedPeripheralsWithServices` asks macOS
//! for the peripherals it is already connected to, which is exactly the set a
//! battery level is wanted for, and blubat never advertises for or discovers
//! a device the system is not already talking to. A peripheral macOS has
//! connected is also already bonded, so a read needs no pairing prompt of its
//! own.
//!
//! The CoreBluetooth flow is strictly sequential and every step of it is
//! bounded: wait for the manager to power on, then per peripheral connect,
//! discover the service, discover the characteristic, read the value and
//! disconnect, each step pumping the run loop until its callback lands or the
//! attempt's own deadline passes. A step that neither lands nor answers
//! usefully is simply no reading for that peripheral this sweep, the
//! one-attempt-no-retry discipline every other daemon sweep keeps.

#![expect(unsafe_code)]

use std::cell::Cell;
use std::time::{Duration, Instant};

use blubat_core::{
    Device, GATT_BATTERY_LEVEL_UUID, GATT_BATTERY_SERVICE_UUID, SweepReading, Timestamp,
    gatt_battery_level, gatt_candidates, gatt_matched,
};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_bluetooth::{
    CBAttribute, CBCentralManager, CBCentralManagerDelegate, CBCharacteristic, CBManagerState,
    CBPeripheral, CBPeripheralDelegate, CBService, CBUUID,
};
use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol, NSString};

/// Somewhere the Battery Service levels of the already connected peripherals
/// are read, which a test fills with a fake.
pub(crate) trait Peripherals {
    /// The battery level of every system connected peripheral whose name is
    /// on `wanted`, paired with that name. Anything else macOS has connected
    /// is left alone, and a peripheral that will not connect, discover or
    /// answer inside `timeout` is simply absent: one attempt, no retry.
    fn levels(&self, wanted: &[String], timeout: Duration) -> Vec<(String, u8)>;
}

/// The real one: CoreBluetooth, on the run loop of the calling thread.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CoreBluetooth;

/// How long one run loop pump may block before the deadline is looked at
/// again, matching how `super::bmap` waits on its own delegate callbacks.
const PUMP: f64 = 0.05;

/// The one thing the delegate callbacks record.
///
/// The flow is strictly sequential with exactly one awaited callback per
/// step, so a single flag says "whatever this attempt was waiting on has
/// landed" without a state machine. Whether it landed usefully is read back
/// off the CoreBluetooth objects themselves rather than out of the callback,
/// which is what keeps this layer as thin as IOBluetooth's.
///
/// Plain interior mutability rather than a lock: everything here runs on the
/// one thread that owns this sweep and pumps the run loop that delivers the
/// callbacks, so nothing is ever contended.
struct Ivars {
    settled: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = Ivars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl CBCentralManagerDelegate for Delegate {
        #[unsafe(method(centralManagerDidUpdateState:))]
        fn central_manager_did_update_state(&self, _central: &CBCentralManager) {
            // The manager's own `state` is read directly while waiting for
            // it, so this required callback exists only to have the run loop
            // wake the wait up.
            self.settle();
        }

        #[unsafe(method(centralManager:didConnectPeripheral:))]
        fn central_manager_did_connect_peripheral(
            &self,
            _central: &CBCentralManager,
            _peripheral: &CBPeripheral,
        ) {
            self.settle();
        }

        #[unsafe(method(centralManager:didFailToConnectPeripheral:error:))]
        fn central_manager_did_fail_to_connect_peripheral_error(
            &self,
            _central: &CBCentralManager,
            _peripheral: &CBPeripheral,
            _error: Option<&NSError>,
        ) {
            self.settle();
        }
    }

    unsafe impl CBPeripheralDelegate for Delegate {
        #[unsafe(method(peripheral:didDiscoverServices:))]
        fn peripheral_did_discover_services(
            &self,
            _peripheral: &CBPeripheral,
            _error: Option<&NSError>,
        ) {
            self.settle();
        }

        #[unsafe(method(peripheral:didDiscoverCharacteristicsForService:error:))]
        fn peripheral_did_discover_characteristics_for_service_error(
            &self,
            _peripheral: &CBPeripheral,
            _service: &CBService,
            _error: Option<&NSError>,
        ) {
            self.settle();
        }

        #[unsafe(method(peripheral:didUpdateValueForCharacteristic:error:))]
        fn peripheral_did_update_value_for_characteristic_error(
            &self,
            _peripheral: &CBPeripheral,
            _characteristic: &CBCharacteristic,
            _error: Option<&NSError>,
        ) {
            self.settle();
        }
    }
);

impl Delegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars {
            settled: Cell::new(false),
        });

        // SAFETY: `init` on a freshly allocated instance of this class,
        // exactly as every other `define_class!` type in this workspace
        // constructs itself.
        unsafe { msg_send![super(this), init] }
    }

    fn settle(&self) {
        self.ivars().settled.set(true);
    }

    /// Runs one step and pumps this thread's run loop until the callback for
    /// it lands or `deadline` passes.
    fn awaiting(&self, deadline: Instant, step: impl FnOnce()) {
        self.ivars().settled.set(false);
        step();

        while !self.ivars().settled.get() && Instant::now() < deadline {
            // SAFETY: pumping the calling thread's own run loop briefly so
            // the delegate callbacks CoreBluetooth queues on it can run.
            unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, PUMP, false) };
        }
    }
}

/// A 16 bit assigned number as CoreBluetooth wants it.
fn uuid(assigned: &str) -> Retained<CBUUID> {
    // SAFETY: a plain constructor over a string CoreBluetooth parses itself.
    unsafe { CBUUID::UUIDWithString(&NSString::from_str(assigned)) }
}

/// The attribute of `uuid` among the ones a discovery left behind.
///
/// A discovery is asked for exactly one UUID, so this is a check that the
/// step answered rather than a search: a peripheral advertising the Battery
/// Service it does not actually serve comes back empty here and is left
/// unreported.
fn with_uuid<T>(found: Option<Retained<NSArray<T>>>, uuid: &CBUUID) -> Option<Retained<T>>
where
    T: objc2::Message + std::ops::Deref<Target = CBAttribute>,
{
    found?
        .to_vec()
        .into_iter()
        // SAFETY: reading an attribute's own UUID, which is what a discovery
        // just filled it in with.
        .find(|attribute| unsafe { *attribute.UUID() == *uuid })
}

/// Whether the manager reached `poweredOn` before `deadline`.
///
/// Bluetooth being off, or the daemon having no TCC grant for it, simply
/// never gets there, and this sweep is given up on in silence rather than
/// reported: the same failure discipline every other step keeps.
fn powered_on(manager: &CBCentralManager, delegate: &Delegate, deadline: Instant) -> bool {
    // SAFETY: reading the manager's own state, which is what its delegate
    // callback exists to announce a change to.
    while unsafe { manager.state() } != CBManagerState::PoweredOn && Instant::now() < deadline {
        delegate.awaiting(deadline, || {});
    }

    // SAFETY: as above.
    unsafe { manager.state() == CBManagerState::PoweredOn }
}

/// One peripheral, connected to and disconnected from again around a single
/// Battery Level read.
///
/// The connection is this attempt's own and is always cancelled, whether or
/// not a level came back, so a sweep never leaves a link open behind it.
fn level(
    manager: &CBCentralManager,
    delegate: &Delegate,
    peripheral: &CBPeripheral,
    battery_service: &CBUUID,
    timeout: Duration,
) -> Option<u8> {
    // SAFETY: the caller's own retained copy is what keeps `delegate` alive,
    // since the property is weak.
    unsafe { peripheral.setDelegate(Some(ProtocolObject::from_ref(delegate))) };

    let read = read(
        manager,
        delegate,
        peripheral,
        battery_service,
        Instant::now() + timeout,
    );

    // SAFETY: cancelling a connection this attempt asked for, whether or not
    // a level ever arrived. Never held open between sweeps.
    unsafe { manager.cancelPeripheralConnection(peripheral) };

    read
}

/// Connect, discover the service, discover the characteristic, read it.
///
/// Every step is bounded by the same `deadline` rather than one of its own,
/// so a peripheral that is merely slow gets whatever the earlier steps did
/// not use, and the attempt as a whole can never outlast one timeout.
fn read(
    manager: &CBCentralManager,
    delegate: &Delegate,
    peripheral: &CBPeripheral,
    battery_service: &CBUUID,
    deadline: Instant,
) -> Option<u8> {
    // SAFETY: connecting to a peripheral macOS already has connected, which
    // is the only kind this sweep ever sees.
    delegate.awaiting(deadline, || unsafe {
        manager.connectPeripheral_options(peripheral, None);
    });

    let services = NSArray::from_slice(&[battery_service]);
    // SAFETY: a discovery filtered to the one service, answered on the
    // delegate this attempt just set.
    delegate.awaiting(deadline, || unsafe {
        peripheral.discoverServices(Some(&services));
    });
    // SAFETY: reading what the discovery above left on the peripheral.
    let service = with_uuid(unsafe { peripheral.services() }, battery_service)?;

    let battery_level = uuid(GATT_BATTERY_LEVEL_UUID);
    let characteristics = NSArray::from_slice(&[&*battery_level]);
    // SAFETY: as above, for the one characteristic inside that service.
    delegate.awaiting(deadline, || unsafe {
        peripheral.discoverCharacteristics_forService(Some(&characteristics), &service);
    });
    // SAFETY: reading what that discovery left on the service.
    let characteristic = with_uuid(unsafe { service.characteristics() }, &battery_level)?;

    // SAFETY: a read of a characteristic this attempt just discovered.
    delegate.awaiting(deadline, || unsafe {
        peripheral.readValueForCharacteristic(&characteristic);
    });

    // SAFETY: reading the value the callback above announced, absent when
    // the read never landed.
    let value = unsafe { characteristic.value() }?;

    gatt_battery_level(&value.to_vec())
}

impl Peripherals for CoreBluetooth {
    fn levels(&self, wanted: &[String], timeout: Duration) -> Vec<(String, u8)> {
        let delegate = Delegate::new();
        // SAFETY: the plain initialiser, which dispatches its events on the
        // main queue, which is the thread this sweep already runs on.
        let manager = unsafe { CBCentralManager::new() };
        // SAFETY: `delegate` outlives `manager` here, which it must, since
        // the delegate property is weak.
        unsafe { manager.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };

        if !powered_on(&manager, &delegate, Instant::now() + timeout) {
            return Vec::new();
        }

        let battery_service = uuid(GATT_BATTERY_SERVICE_UUID);
        let services = NSArray::from_slice(&[&*battery_service]);
        // SAFETY: asking macOS which peripherals it has already connected
        // that serve this service. Never a scan.
        let connected = unsafe { manager.retrieveConnectedPeripheralsWithServices(&services) };

        connected
            .to_vec()
            .into_iter()
            .filter_map(|peripheral| {
                // SAFETY: reading the name macOS shows for the peripheral,
                // which is what `blubat_core::gatt` matches a device on.
                let name = unsafe { peripheral.name() }?.to_string();
                if !wanted.contains(&name) {
                    return None;
                }

                let level = level(&manager, &delegate, &peripheral, &battery_service, timeout)?;

                Some((name, level))
            })
            .collect()
    }
}

/// Runs the sweep against every peripheral `devices` still wants a level for.
///
/// A failure on one peripheral never stops the rest: a refused connection, a
/// service that is not there, a timeout or a value blubat cannot read are all
/// silently no reading for that device this sweep. A peripheral whose name
/// matches no device is skipped without ever being connected to.
pub(crate) fn sweep(
    peripherals: &dyn Peripherals,
    devices: &[Device],
    read_at: Timestamp,
    timeout: Duration,
) -> Vec<SweepReading> {
    let wanted = gatt_candidates(devices);
    if wanted.is_empty() {
        return Vec::new();
    }

    peripherals
        .levels(&wanted, timeout)
        .into_iter()
        .filter_map(|(name, level)| {
            gatt_matched(devices, &name)
                .map(|address| SweepReading::gatt(address, name, level, read_at))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use blubat_core::{Address, ChargeState, Levels, Source};

    use super::*;

    const KEYS: &str = "de-df-38-f0-46-9b";
    const READ_AT: Timestamp = Timestamp::from_unix(1_785_643_199);

    fn address(raw: &str) -> Address {
        Address::parse(raw).expect("valid address")
    }

    fn device(name: &str, raw: &str, source: Source) -> Device {
        Device {
            address: address(raw),
            name: name.to_string(),
            kind: None,
            transport: None,
            vendor_id: None,
            product_id: None,
            levels: Levels::default(),
            charge: ChargeState::Unknown,
            source,
            connected: true,
            read_at: READ_AT,
        }
    }

    fn keys() -> Device {
        device("MX Keys M Mac", KEYS, Source::SystemProfiler)
    }

    /// Peripherals that answer with whatever was queued for them, and record
    /// the names each sweep asked about.
    #[derive(Default)]
    struct Fake {
        answers: Vec<(String, u8)>,
        asked: Mutex<Vec<Vec<String>>>,
    }

    impl Fake {
        fn answering(answers: Vec<(&str, u8)>) -> Self {
            Self {
                answers: answers
                    .into_iter()
                    .map(|(name, level)| (name.to_string(), level))
                    .collect(),
                asked: Mutex::new(Vec::new()),
            }
        }
    }

    impl Peripherals for Fake {
        fn levels(&self, wanted: &[String], _timeout: Duration) -> Vec<(String, u8)> {
            self.asked
                .lock()
                .expect("an unpoisoned fake")
                .push(wanted.to_vec());

            self.answers
                .iter()
                .filter(|(name, _)| wanted.contains(name))
                .cloned()
                .collect()
        }
    }

    #[test]
    fn a_peripheral_that_answers_becomes_one_reading_under_its_devices_address() {
        let fake = Fake::answering(vec![("MX Keys M Mac", 95)]);

        let readings = sweep(&fake, &[keys()], READ_AT, Duration::ZERO);

        assert_eq!(
            readings,
            [SweepReading::gatt(
                address(KEYS),
                "MX Keys M Mac",
                95,
                READ_AT
            )]
        );
        assert_eq!(
            fake.asked.lock().unwrap().as_slice(),
            [vec!["MX Keys M Mac".to_string()]]
        );
    }

    #[test]
    fn a_peripheral_matching_no_device_is_no_reading_at_all() {
        let fake = Fake::answering(vec![("Someone Elses Keyboard", 60)]);

        assert_eq!(sweep(&fake, &[keys()], READ_AT, Duration::ZERO), []);
    }

    #[test]
    fn nothing_is_asked_for_when_every_device_already_has_a_level() {
        let reported = Device {
            levels: Levels {
                main: Some(40),
                ..Levels::default()
            },
            ..keys()
        };
        let fake = Fake::answering(vec![("MX Keys M Mac", 95)]);

        assert_eq!(sweep(&fake, &[reported], READ_AT, Duration::ZERO), []);
        assert!(
            fake.asked.lock().unwrap().is_empty(),
            "a device with a direct level is never even asked about"
        );
    }

    #[test]
    fn one_peripheral_that_did_not_answer_does_not_stop_the_rest_of_the_sweep() {
        let keychron = device("Keychron K3", "aa-bb-cc-dd-ee-ff", Source::SystemProfiler);
        let fake = Fake::answering(vec![("Keychron K3", 50)]);

        let readings = sweep(&fake, &[keys(), keychron], READ_AT, Duration::ZERO);

        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].name, "Keychron K3");
        assert_eq!(readings[0].level, 50);
    }

    #[test]
    fn a_device_gatt_already_took_over_is_swept_again_so_its_level_keeps_moving() {
        let taken_over = Device {
            source: Source::Gatt,
            levels: Levels {
                main: Some(40),
                ..Levels::default()
            },
            ..keys()
        };
        let fake = Fake::answering(vec![("MX Keys M Mac", 95)]);

        let readings = sweep(&fake, &[taken_over], READ_AT, Duration::ZERO);

        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].level, 95);
    }
}
