//! Run an rs-matter device on Android.
//!
//! The Rust side is the Matter node (rs-matter): commissioning, fabrics, the data model. The Kotlin
//! side (`org.rsmatter.android.MatterNode`, in `android/`) owns everything Android: the
//! `BluetoothGattServer` and advertiser that carry BLE commissioning, the app's storage, and what to
//! do with the network credentials a commissioner sends (a Thread operational dataset, or Wi-Fi
//! credentials). The node itself runs over the IP network Android is already on.
//!
//! The JNI functions below are the Kotlin side's `external fun`s.

#![allow(clippy::missing_safety_doc)]

mod bridge;
mod endpoints;
mod gatt;
mod netctl;
mod node;
mod scan;

pub use gatt::AndroidGattPeripheral;
pub use netctl::AndroidNetCtl;

use std::path::PathBuf;
use std::sync::Arc;

use jni::objects::{JByteArray, JObject, JString};
use jni::sys::{jboolean, jint, jlong, JNI_TRUE};
use jni::JNIEnv;
use log::{error, info, warn};

use bridge::{bt_addr, C1Write, Shared};

/// The node thread's stack: rs-matter's futures are large (the wireless test runner uses 550 KiB).
const NODE_STACK_SIZE: usize = 1024 * 1024;

fn init_logging() {
    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Debug)
            // Each command a controller invokes (rs_matter_im::invoker) is logged at debug level.
            .with_filter(android_logger::FilterBuilder::new().parse("info,rs_matter_im::invoker=debug").build())
            .with_tag("rs-matter"),
    );
}

/// `MatterNode.nativeStart(storageDir, discriminator, passcode, deviceName, serial, onNetworkCommissioning): Long`.
/// Starts the node on its own thread; returns the handle the other calls take, or 0.
#[no_mangle]
pub extern "system" fn Java_org_rsmatter_android_MatterNode_nativeStart<'l>(
    mut env: JNIEnv<'l>,
    this: JObject<'l>,
    storage_dir: JString<'l>,
    discriminator: jint,
    passcode: jlong,
    device_name: JString<'l>,
    serial: JString<'l>,
    on_network_commissioning: jboolean,
    endpoints: jni::objects::JIntArray<'l>,
) -> jlong {
    init_logging();
    let strings = (|| -> jni::errors::Result<(String, String, String)> {
        Ok((
            env.get_string(&storage_dir)?.into(),
            env.get_string(&device_name)?.into(),
            env.get_string(&serial)?.into(),
        ))
    })();
    let Ok((dir, name, serial)) = strings else { return 0 };
    // [id, kind, id, kind, …] (MatterNode.Endpoint)
    let eps = {
        let len = env.get_array_length(&endpoints).unwrap_or(0).max(0) as usize;
        let mut raw = vec![0i32; len];
        if env.get_int_array_region(&endpoints, 0, &mut raw).is_err() {
            return 0;
        }
        let mut eps = Vec::new();
        for pair in raw.chunks_exact(2) {
            match (u16::try_from(pair[0]), endpoints::Kind::from_i32(pair[1])) {
                (Ok(id), Some(kind)) if id >= 1 => eps.push((id, kind)),
                _ => {
                    error!("Bad endpoint {:?}", pair);
                    return 0;
                }
            }
        }
        eps
    };
    let shared = match Shared::new(&mut env, &this) {
        Ok(s) => s,
        Err(e) => {
            error!("JNI: cannot set up the node: {e}");
            return 0;
        }
    };
    let cfg = node::NodeConfig {
        storage_dir: PathBuf::from(dir),
        discriminator: (discriminator as u32 & 0xfff) as u16,
        passcode: passcode as u32,
        device_name: name,
        serial,
        on_network_commissioning: on_network_commissioning == JNI_TRUE,
        endpoints: eps,
    };

    scan::register(shared.clone());
    let thread_shared = shared.clone();
    let spawned = std::thread::Builder::new()
        .name("rs-matter".into())
        .stack_size(NODE_STACK_SIZE)
        .spawn(move || {
            info!("Matter node starting");
            let r = node::run(thread_shared.clone(), cfg);
            match &r {
                Ok(()) => thread_shared.event("stopped", ""),
                Err(e) => {
                    error!("Matter node failed: {e:?}");
                    thread_shared.event("stopped", &format!("{e:?}"));
                }
            }
        });
    if let Err(e) = spawned {
        error!("Cannot start the node thread: {e}");
        return 0;
    }
    Arc::into_raw(shared) as jlong
}

/// Borrows the [`Shared`] behind a handle without taking ownership.
unsafe fn shared<'a>(handle: jlong) -> Option<&'a Shared> {
    (handle != 0).then(|| unsafe { &*(handle as *const Shared) })
}

/// `MatterNode.nativeStop(handle)`: stops the node and releases the handle.
#[no_mangle]
pub unsafe extern "system" fn Java_org_rsmatter_android_MatterNode_nativeStop(_env: JNIEnv, _this: JObject, handle: jlong) {
    if handle == 0 {
        return;
    }
    let s = unsafe { Arc::from_raw(handle as *const Shared) };
    let _ = s.stop.0.try_send(());
}

/// `MatterNode.nativeC1Write(handle, addr, mtu, value)`: a peer wrote to C1 (already acknowledged).
#[no_mangle]
pub unsafe extern "system" fn Java_org_rsmatter_android_MatterNode_nativeC1Write<'l>(
    env: JNIEnv<'l>,
    _this: JObject<'l>,
    handle: jlong,
    addr: JByteArray<'l>,
    mtu: jint,
    value: JByteArray<'l>,
) {
    let Some(s) = (unsafe { shared(handle) }) else { return };
    let (Ok(a), Ok(v)) = (env.convert_byte_array(&addr), env.convert_byte_array(&value)) else { return };
    let Some(addr) = bt_addr(&a) else { return };
    if s.c1.0.try_send(C1Write { addr, mtu: mtu.clamp(23, u16::MAX as jint) as u16, value: v }).is_err() {
        warn!("Dropping a write to C1: queue full");
    }
}

/// `MatterNode.nativeC2Subscribed(handle, addr, subscribed)`: a peer (un)subscribed to C2
/// indications, or disconnected (`subscribed` false).
#[no_mangle]
pub unsafe extern "system" fn Java_org_rsmatter_android_MatterNode_nativeC2Subscribed<'l>(
    env: JNIEnv<'l>,
    _this: JObject<'l>,
    handle: jlong,
    addr: JByteArray<'l>,
    subscribed: jboolean,
) {
    let Some(s) = (unsafe { shared(handle) }) else { return };
    let Ok(a) = env.convert_byte_array(&addr) else { return };
    if let Some(addr) = bt_addr(&a) {
        let _ = s.c2.0.try_send((addr, subscribed == JNI_TRUE));
    }
}

/// `MatterNode.nativeIndicated(handle, ok)`: the last C2 indication was confirmed (or failed).
#[no_mangle]
pub unsafe extern "system" fn Java_org_rsmatter_android_MatterNode_nativeIndicated(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    ok: jboolean,
) {
    if let Some(s) = unsafe { shared(handle) } {
        let _ = s.indicated.0.try_send(ok == JNI_TRUE);
    }
}

/// `MatterNode.nativeSetValue(handle, endpoint, value)`: the app's new value for an endpoint
/// (On/Off 0/1, or a sensor reading in Matter units; see `endpoints::Kind`).
#[no_mangle]
pub unsafe extern "system" fn Java_org_rsmatter_android_MatterNode_nativeSetValue(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    endpoint: jint,
    value: jint,
) {
    if let (Some(s), Ok(e)) = (unsafe { shared(handle) }, u16::try_from(endpoint)) {
        let _ = s.values.0.try_send((e, value));
    }
}

/// `MatterNode.nativeOpenCommissioningWindow(handle)`: open the basic commissioning window again.
#[no_mangle]
pub unsafe extern "system" fn Java_org_rsmatter_android_MatterNode_nativeOpenCommissioningWindow(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) {
    if let Some(s) = unsafe { shared(handle) } {
        let _ = s.commands.0.try_send(node::Command::OpenCommissioningWindow);
    }
}
