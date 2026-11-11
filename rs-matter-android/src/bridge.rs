//! The bridge between the Rust node and its Kotlin side (`org.rsmatter.android.MatterNode`).
//!
//! Kotlin owns everything Android: the `BluetoothGattServer` and the advertiser, and what to do with
//! network credentials. It reports Bluetooth events through the `native*` functions in `lib.rs`,
//! which land on the channels here; the node's thread calls back into Kotlin through [`Shared`].

use std::sync::Arc;

use async_channel::{Receiver, Sender};
use jni::objects::{GlobalRef, JObject, JValue};
use jni::{JNIEnv, JavaVM};
use log::warn;

use rs_matter::error::{Error, ErrorCode};
use rs_matter::transport::network::BtAddr;

/// A write to C1, already acknowledged to the peer by the Kotlin side.
pub struct C1Write {
    pub addr: BtAddr,
    pub mtu: u16,
    pub value: Vec<u8>,
}

/// State shared by the node's thread and the JNI callbacks (the `long` handle Kotlin holds).
pub struct Shared {
    vm: JavaVM,
    node: GlobalRef,
    pub(crate) c1: (Sender<C1Write>, Receiver<C1Write>),
    /// (peer, subscribed) for C2 indications.
    pub(crate) c2: (Sender<(BtAddr, bool)>, Receiver<(BtAddr, bool)>),
    /// The outcome of the last indication (`onNotificationSent`).
    pub(crate) indicated: (Sender<bool>, Receiver<bool>),
    /// Kotlin asked the node to stop.
    pub(crate) stop: (Sender<()>, Receiver<()>),
    /// The app switched the On/Off endpoint itself (`MatterNode.setOnOff`).
    pub(crate) on_off_updates: (Sender<bool>, Receiver<bool>),
}

/// C1 writes waiting for the BTP session; the first one comes before the peer subscribes to C2.
const C1_WRITES_QUEUED: usize = 8;

impl Shared {
    pub fn new(env: &mut JNIEnv, node: &JObject) -> jni::errors::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            vm: env.get_java_vm()?,
            node: env.new_global_ref(node)?,
            c1: async_channel::bounded(C1_WRITES_QUEUED),
            c2: async_channel::unbounded(),
            indicated: async_channel::bounded(1),
            stop: async_channel::bounded(1),
            on_off_updates: async_channel::unbounded(),
        }))
    }

    /// Runs `f` with a JNI environment for the current thread (attached for good: the node runs
    /// on one long-lived thread). A pending Java exception is logged and cleared.
    fn with_env<R>(
        &self,
        what: &str,
        f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<R>,
    ) -> Result<R, Error> {
        let mut env = self.vm.attach_current_thread_permanently().map_err(|e| {
            warn!("JNI: cannot attach to {what}: {e}");
            Error::from(ErrorCode::InvalidState)
        })?;
        let r = f(&mut env, self.node.as_obj());
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
        r.map_err(|e| {
            warn!("JNI: {what} failed: {e}");
            ErrorCode::InvalidState.into()
        })
    }

    /// `bleStart(serviceData): String?`: publish the Matter GATT service and advertise it.
    pub fn ble_start(&self, service_data: &[u8]) -> Result<(), Error> {
        let err: Option<String> = self.with_env("bleStart", |env, node| {
            let data = env.byte_array_from_slice(service_data)?;
            let r = env
                .call_method(node, "bleStart", "([B)Ljava/lang/String;", &[JValue::Object(&data)])?
                .l()?;
            if r.is_null() {
                Ok(None)
            } else {
                Ok(Some(env.get_string(&r.into())?.into()))
            }
        })?;
        match err {
            None => Ok(()),
            Some(why) => {
                warn!("Bluetooth: cannot run the GATT peripheral: {why}");
                Err(ErrorCode::NoNetworkInterface.into())
            }
        }
    }

    /// `bleStop()`: stop advertising and close the GATT server.
    pub fn ble_stop(&self) {
        let _ = self.with_env("bleStop", |env, node| env.call_method(node, "bleStop", "()V", &[]).map(|_| ()));
    }

    /// `bleIndicate(addr, value): Boolean`: indicate `value` on C2 to `addr`. When it returns
    /// true, Kotlin reports the outcome through `nativeIndicated`.
    pub fn ble_indicate(&self, addr: BtAddr, value: &[u8]) -> Result<bool, Error> {
        self.with_env("bleIndicate", |env, node| {
            let a = env.byte_array_from_slice(&addr.0)?;
            let v = env.byte_array_from_slice(value)?;
            env.call_method(node, "bleIndicate", "([B[B)Z", &[JValue::Object(&a), JValue::Object(&v)])?
                .z()
        })
    }

    /// `bleDisconnect(addr)`: drop the connection of `addr`.
    pub fn ble_disconnect(&self, addr: BtAddr) {
        let _ = self.with_env("bleDisconnect", |env, node| {
            let a = env.byte_array_from_slice(&addr.0)?;
            env.call_method(node, "bleDisconnect", "([B)V", &[JValue::Object(&a)]).map(|_| ())
        });
    }

    /// `onThreadDataset(dataset): Boolean`: the commissioner's Thread operational dataset.
    pub fn thread_dataset(&self, dataset: &[u8]) -> Result<bool, Error> {
        self.with_env("onThreadDataset", |env, node| {
            let d = env.byte_array_from_slice(dataset)?;
            env.call_method(node, "onThreadDataset", "([B)Z", &[JValue::Object(&d)])?.z()
        })
    }

    /// `onWifiCredentials(ssid, pass): Boolean`.
    pub fn wifi_credentials(&self, ssid: &[u8], pass: &[u8]) -> Result<bool, Error> {
        self.with_env("onWifiCredentials", |env, node| {
            let s = env.byte_array_from_slice(ssid)?;
            let p = env.byte_array_from_slice(pass)?;
            env.call_method(node, "onWifiCredentials", "([B[B)Z", &[JValue::Object(&s), JValue::Object(&p)])?
                .z()
        })
    }

    /// `onThreadScan(): ByteArray`: the Thread networks the app sees, packed by
    /// `MatterNode.packThreadScan` (see [`crate::scan`]).
    pub fn thread_scan(&self) -> Result<Vec<u8>, Error> {
        self.with_env("onThreadScan", |env, node| {
            let r = env.call_method(node, "onThreadScan", "()[B", &[])?.l()?;
            if r.is_null() {
                Ok(Vec::new())
            } else {
                env.convert_byte_array(&jni::objects::JByteArray::from(r))
            }
        })
    }

    /// `onOnOff(on)`: a controller switched the On/Off endpoint.
    pub fn on_off(&self, on: bool) {
        let _ = self.with_env("onOnOff", |env, node| {
            env.call_method(node, "onOnOff", "(Z)V", &[JValue::Bool(on.into())]).map(|_| ())
        });
    }

    /// `onEvent(kind, detail)`: what the node is doing (pairing codes, commissioned, stopped…).
    pub fn event(&self, kind: &str, detail: &str) {
        let _ = self.with_env("onEvent", |env, node| {
            let k = env.new_string(kind)?;
            let d = env.new_string(detail)?;
            env.call_method(
                node,
                "onEvent",
                "(Ljava/lang/String;Ljava/lang/String;)V",
                &[JValue::Object(&k), JValue::Object(&d)],
            )
            .map(|_| ())
        });
    }
}

/// A Bluetooth address as Kotlin passes it (6 bytes, most significant first).
pub fn bt_addr(bytes: &[u8]) -> Option<BtAddr> {
    let a: [u8; 6] = bytes.try_into().ok()?;
    Some(BtAddr(a))
}
