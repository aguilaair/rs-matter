//! The app's endpoints, chosen at runtime from Kotlin (`MatterNode.Endpoint`): On/Off, and sensors
//! whose values the app pushes (`MatterNode.setValue`). rs-matter's data model is otherwise built at
//! compile time; here the node's endpoint list is built (and leaked, once per process) from the
//! app's configuration, and one handler routes every app endpoint to its cluster handler.

use core::future::Future;
use core::pin::Pin;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use async_channel::{Receiver, Sender};
use log::{info, warn};

use rs_matter::dm::clusters::app::on_off::{self, EffectVariantEnum, OnOffHooks, OutOfBandMessage};
use rs_matter::dm::clusters::decl::illuminance_measurement as illum;
use rs_matter::dm::clusters::decl::occupancy_sensing as occ;
use rs_matter::dm::clusters::decl::on_off as on_off_cluster;
use rs_matter::dm::clusters::decl::relative_humidity_measurement as hum;
use rs_matter::dm::clusters::decl::temperature_measurement as temp;
use rs_matter::dm::clusters::desc::{self, ClusterHandler as _};
use rs_matter::dm::clusters::groups::{self, ClusterHandler as _};
use rs_matter::dm::devices::{
    DEV_TYPE_HUMIDITY_SENSOR, DEV_TYPE_LIGHT_SENSOR, DEV_TYPE_OCCUPANCY_SENSOR, DEV_TYPE_ON_OFF_LIGHT,
    DEV_TYPE_TEMPERATURE_SENSOR,
};
use rs_matter::dm::{
    AttrId, Cluster, ClusterId, Dataver, EndptId, Endpoint, Handler, HandlerContext, InvokeContext, InvokeReply,
    LifecycleOp, MatchContext, ReadContext, ReadReply, WriteContext,
};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::Nullable;
use rs_matter::utils::sync::MaybeSend;
use rs_matter::{clusters, devices, with};

use crate::bridge::Shared;

/// A value the app hasn't set yet (reads as null).
pub const UNKNOWN: i32 = i32::MIN;

/// What an app endpoint is. The numbers are `MatterNode.Endpoint.kind` on the Kotlin side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// On/Off light: commands go to `Listener.onOnOff(endpoint, on)`; value 0/1 from the app.
    OnOff = 1,
    /// Occupancy sensor: value 0/1.
    Occupancy = 2,
    /// Light sensor: value in Matter units, `10000 * log10(lux) + 1` (Kotlin converts from lux).
    Light = 3,
    /// Temperature sensor: value in 0.01 °C.
    Temperature = 4,
    /// Humidity sensor: value in 0.01 % RH.
    Humidity = 5,
}

impl Kind {
    pub fn from_i32(k: i32) -> Option<Self> {
        Some(match k {
            1 => Self::OnOff,
            2 => Self::Occupancy,
            3 => Self::Light,
            4 => Self::Temperature,
            5 => Self::Humidity,
            _ => return None,
        })
    }

    /// The cluster and attribute a pushed value changes.
    fn value_attr(self) -> (ClusterId, AttrId) {
        match self {
            Self::OnOff => (on_off_cluster::FULL_CLUSTER.id, on_off_cluster::AttributeId::OnOff as _),
            Self::Occupancy => (occ::FULL_CLUSTER.id, occ::AttributeId::Occupancy as _),
            Self::Light => (illum::FULL_CLUSTER.id, illum::AttributeId::MeasuredValue as _),
            Self::Temperature => (temp::FULL_CLUSTER.id, temp::AttributeId::MeasuredValue as _),
            Self::Humidity => (hum::FULL_CLUSTER.id, hum::AttributeId::MeasuredValue as _),
        }
    }

    fn endpoint(self, id: EndptId) -> Endpoint<'static> {
        match self {
            Self::OnOff => Endpoint::new(
                id,
                devices!(DEV_TYPE_ON_OFF_LIGHT),
                clusters!(desc::DescHandler::CLUSTER, groups::GroupsHandler::CLUSTER, KotlinOnOff::CLUSTER),
            ),
            Self::Occupancy => Endpoint::new(id, devices!(DEV_TYPE_OCCUPANCY_SENSOR), clusters!(desc::DescHandler::CLUSTER, <Occupancy<'static> as occ::ClusterHandler>::CLUSTER)),
            Self::Light => Endpoint::new(id, devices!(DEV_TYPE_LIGHT_SENSOR), clusters!(desc::DescHandler::CLUSTER, <Light<'static> as illum::ClusterHandler>::CLUSTER)),
            Self::Temperature => Endpoint::new(id, devices!(DEV_TYPE_TEMPERATURE_SENSOR), clusters!(desc::DescHandler::CLUSTER, <Temperature<'static> as temp::ClusterHandler>::CLUSTER)),
            Self::Humidity => Endpoint::new(id, devices!(DEV_TYPE_HUMIDITY_SENSOR), clusters!(desc::DescHandler::CLUSTER, <Humidity<'static> as hum::ClusterHandler>::CLUSTER)),
        }
    }
}

/// The app's endpoints as `rs-matter` endpoint metadata, after the root endpoint.
pub fn metadata(eps: &[(EndptId, Kind)]) -> Vec<Endpoint<'static>> {
    eps.iter().map(|(id, k)| k.endpoint(*id)).collect()
}

/// One sensor endpoint's value and data version.
pub struct Measure {
    dataver: Dataver,
    value: AtomicI32,
}

impl Measure {
    fn get(&self) -> Option<i32> {
        let v = self.value.load(Ordering::Relaxed);
        (v != UNKNOWN).then_some(v)
    }
}

/// Serves every app endpoint: picks the endpoint's cluster handler at runtime.
pub struct AppEndpoints<'a> {
    slots: Vec<Slot<'a>>,
    updates: Receiver<(EndptId, i32)>,
}

enum Slot<'a> {
    OnOff(EndptId, on_off::OnOffHandler<'a, KotlinOnOff, on_off::NoLevelControl>, Sender<bool>),
    Sensor(EndptId, Kind, Measure),
}

impl Slot<'_> {
    fn id(&self) -> EndptId {
        match self {
            Self::OnOff(id, ..) | Self::Sensor(id, ..) => *id,
        }
    }
}

impl<'a> AppEndpoints<'a> {
    /// `kv_base`: the first KV key for the On/Off endpoints' persisted state (one key each).
    pub fn new(
        eps: &[(EndptId, Kind)],
        shared: Arc<Shared>,
        updates: Receiver<(EndptId, i32)>,
        rand: &mut impl rand::Rng,
        kv_base: u16,
    ) -> Self {
        let mut slots = Vec::new();
        for (i, (id, kind)) in eps.iter().enumerate() {
            let slot = match kind {
                Kind::OnOff => {
                    let (tx, rx) = async_channel::unbounded();
                    let hooks = KotlinOnOff { shared: shared.clone(), endpoint: *id, updates: rx };
                    let h = on_off::OnOffHandler::new_standalone(Dataver::new_rand(rand), *id, kv_base + i as u16, hooks);
                    Slot::OnOff(*id, h, tx)
                }
                k => Slot::Sensor(*id, *k, Measure { dataver: Dataver::new_rand(rand), value: AtomicI32::new(UNKNOWN) }),
            };
            slots.push(slot);
        }
        Self { slots, updates }
    }

    fn slot(&self, endpoint: Option<EndptId>) -> Result<&Slot<'a>, Error> {
        let e = endpoint.ok_or(ErrorCode::EndpointNotFound)?;
        self.slots.iter().find(|s| s.id() == e).ok_or_else(|| ErrorCode::EndpointNotFound.into())
    }

    /// A value the app pushed: stored, and subscribers told.
    fn apply(&self, ctx: &impl HandlerContext, endpoint: EndptId, value: i32) {
        let Some(slot) = self.slots.iter().find(|s| s.id() == endpoint) else {
            warn!("No app endpoint {endpoint}");
            return;
        };
        match slot {
            Slot::OnOff(_, _, tx) => {
                let _ = tx.try_send(value != 0);
            }
            Slot::Sensor(id, kind, m) => {
                if m.value.swap(value, Ordering::Relaxed) != value {
                    m.dataver.changed();
                    let (c, a) = kind.value_attr();
                    ctx.notify_attr_changed(*id, c, a);
                }
            }
        }
    }
}

/// Runs one closure per kind of cluster handler for `slot` (a macro: the handler types differ).
macro_rules! dispatch {
    ($slot:expr, $h:ident => $body:expr) => {
        match $slot {
            Slot::OnOff(_, oo, _) => {
                let $h = on_off::HandlerAdaptor(oo);
                $body
            }
            Slot::Sensor(_, Kind::Occupancy, m) => {
                let $h = occ::HandlerAdaptor(Occupancy(m));
                $body
            }
            Slot::Sensor(_, Kind::Light, m) => {
                let $h = illum::HandlerAdaptor(Light(m));
                $body
            }
            Slot::Sensor(_, Kind::Temperature, m) => {
                let $h = temp::HandlerAdaptor(Temperature(m));
                $body
            }
            Slot::Sensor(_, Kind::Humidity, m) => {
                let $h = hum::HandlerAdaptor(Humidity(m));
                $body
            }
            Slot::Sensor(_, Kind::OnOff, _) => Err(ErrorCode::ClusterNotFound.into()),
        }
    };
}

impl rs_matter::dm::NonBlockingHandler for AppEndpoints<'_> {}

impl Handler for AppEndpoints<'_> {
    fn read(&self, ctx: impl ReadContext, reply: impl ReadReply) -> Result<(), Error> {
        let slot = self.slot(ctx.endpt())?;
        dispatch!(slot, h => h.read(ctx, reply))
    }

    fn write(&self, ctx: impl WriteContext) -> Result<(), Error> {
        let slot = self.slot(ctx.endpt())?;
        dispatch!(slot, h => h.write(ctx))
    }

    fn invoke(&self, ctx: impl InvokeContext, reply: impl InvokeReply) -> Result<(), Error> {
        let slot = self.slot(ctx.endpt())?;
        dispatch!(slot, h => h.invoke(ctx, reply))
    }

    fn bump_dataver(&self, ctx: impl MatchContext) {
        if let Ok(slot) = self.slot(ctx.endpt()) {
            let _: Result<(), Error> = dispatch!(slot, h => {
                h.bump_dataver(ctx);
                Ok(())
            });
        }
    }

    fn lifecycle(&self, ctx: impl HandlerContext, op: LifecycleOp) -> Result<(), Error> {
        // Unlike read/write/invoke, every handler gets it: each app endpoint's (On/Off restores its
        // persisted state on Startup).
        for slot in &self.slots {
            dispatch!(slot, h => h.lifecycle(&ctx, op.clone()))?;
        }
        Ok(())
    }

    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        let ctx = &ctx;
        // The On/Off handlers' own background work (their hooks follow the app's updates)…
        let mut tasks: Vec<Pin<Box<dyn Future<Output = Result<(), Error>> + '_>>> = self
            .slots
            .iter()
            .filter_map(|s| match s {
                Slot::OnOff(_, oo, _) => Some(Box::pin(on_off_cluster::ClusterHandler::run(oo, ctx)) as _),
                _ => None,
            })
            .collect();
        // …and the values the app pushes.
        tasks.push(Box::pin(async move {
            while let Ok((endpoint, value)) = self.updates.recv().await {
                self.apply(ctx, endpoint, value);
            }
            core::future::pending::<()>().await;
            Ok(())
        }));
        rs_matter::utils::futures::select_slice(Pin::new(&mut tasks[..])).await.0
    }
}

// ---- the cluster handlers

fn nullable<T>(m: &Measure, f: impl FnOnce(i32) -> T) -> Nullable<T> {
    m.get().map(|v| Nullable::some(f(v))).unwrap_or_else(Nullable::none)
}

struct Occupancy<'a>(&'a Measure);

impl occ::ClusterHandler for Occupancy<'_> {
    // A presence sensor of the "active infrared" kind (time-of-flight), which the legacy sensor
    // type reports as PIR (Matter Application Cluster spec 2.7.5).
    const CLUSTER: Cluster<'static> = occ::FULL_CLUSTER
        .with_features(occ::Feature::ACTIVE_INFRARED.bits())
        .with_attrs(with!(required));

    fn dataver(&self) -> u32 {
        self.0.dataver.get()
    }
    fn dataver_changed(&self) {
        self.0.dataver.changed();
    }
    fn occupancy(&self, _ctx: impl ReadContext) -> Result<occ::OccupancyBitmap, Error> {
        Ok(if self.0.get() == Some(1) { occ::OccupancyBitmap::OCCUPIED } else { occ::OccupancyBitmap::empty() })
    }
    fn occupancy_sensor_type(&self, _ctx: impl ReadContext) -> Result<occ::OccupancySensorTypeEnum, Error> {
        Ok(occ::OccupancySensorTypeEnum::PIR)
    }
    fn occupancy_sensor_type_bitmap(&self, _ctx: impl ReadContext) -> Result<occ::OccupancySensorTypeBitmap, Error> {
        Ok(occ::OccupancySensorTypeBitmap::PIR)
    }
}

struct Light<'a>(&'a Measure);

impl illum::ClusterHandler for Light<'_> {
    const CLUSTER: Cluster<'static> = illum::FULL_CLUSTER.with_attrs(with!(required));

    fn dataver(&self) -> u32 {
        self.0.dataver.get()
    }
    fn dataver_changed(&self) {
        self.0.dataver.changed();
    }
    fn measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(nullable(self.0, |v| v.clamp(0, 0xfffe) as u16))
    }
    fn min_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::none())
    }
    fn max_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::none())
    }
}

struct Temperature<'a>(&'a Measure);

impl temp::ClusterHandler for Temperature<'_> {
    const CLUSTER: Cluster<'static> = temp::FULL_CLUSTER.with_attrs(with!(required));

    fn dataver(&self) -> u32 {
        self.0.dataver.get()
    }
    fn dataver_changed(&self) {
        self.0.dataver.changed();
    }
    fn measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(nullable(self.0, |v| v.clamp(-27315, 32767) as i16))
    }
    fn min_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(Nullable::none())
    }
    fn max_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(Nullable::none())
    }
}

struct Humidity<'a>(&'a Measure);

impl hum::ClusterHandler for Humidity<'_> {
    const CLUSTER: Cluster<'static> = hum::FULL_CLUSTER.with_attrs(with!(required));

    fn dataver(&self) -> u32 {
        self.0.dataver.get()
    }
    fn dataver_changed(&self) {
        self.0.dataver.changed();
    }
    fn measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(nullable(self.0, |v| v.clamp(0, 10000) as u16))
    }
    fn min_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::none())
    }
    fn max_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::none())
    }
}

/// On/Off whose state lives in the app: commands go to Kotlin `onOnOff(endpoint, on)`, and the app's
/// own changes (`MatterNode.setOnOff`) come back as out-of-band updates.
pub struct KotlinOnOff {
    shared: Arc<Shared>,
    endpoint: EndptId,
    updates: Receiver<bool>,
}

impl OnOffHooks for KotlinOnOff {
    const CLUSTER: Cluster<'static> = on_off_cluster::FULL_CLUSTER
        .with_revision(6)
        .with_features(on_off_cluster::Feature::LIGHTING.bits())
        .with_attrs(with!(
            required;
            on_off_cluster::AttributeId::OnOff
                | on_off_cluster::AttributeId::GlobalSceneControl
                | on_off_cluster::AttributeId::OnTime
                | on_off_cluster::AttributeId::OffWaitTime
                | on_off_cluster::AttributeId::StartUpOnOff
        ))
        .with_cmds(with!(
            on_off_cluster::CommandId::Off
                | on_off_cluster::CommandId::On
                | on_off_cluster::CommandId::Toggle
                | on_off_cluster::CommandId::OffWithEffect
                | on_off_cluster::CommandId::OnWithRecallGlobalScene
                | on_off_cluster::CommandId::OnWithTimedOff
        ));

    fn set_on_off(&self, on: bool) {
        self.shared.on_off(self.endpoint, on);
    }

    async fn handle_off_with_effect(&self, _effect: EffectVariantEnum) {}

    async fn run<F: Fn(OutOfBandMessage) + MaybeSend>(&self, notify: F) {
        loop {
            match self.updates.recv().await {
                Ok(on) => notify(OutOfBandMessage::Update(on)),
                Err(_) => core::future::pending::<()>().await,
            }
        }
    }
}

/// Logs the configured endpoints (once, at start).
pub fn log(eps: &[(EndptId, Kind)]) {
    info!("App endpoints: {:?}", eps);
}
