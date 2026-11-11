//! The node: an rs-matter stack commissioned over BLE (Android's GATT server) and run over
//! Android's IP network, with the credentials it receives handed to Kotlin.

use core::mem::MaybeUninit;
use core::pin::pin;
use std::path::PathBuf;
use std::sync::Arc;

use log::info;

use rs_matter::crypto::{default_crypto, Crypto};
use rs_matter::dm::clusters::app::level_control::LevelControlHooks;
use rs_matter::dm::clusters::app::on_off::{self, EffectVariantEnum, OnOffHooks, OutOfBandMessage};
use rs_matter::dm::clusters::basic_info::BasicInfoConfig;
use rs_matter::dm::clusters::decl::on_off as on_off_cluster;
use rs_matter::dm::clusters::decl::on_off::Feature;
use rs_matter::dm::clusters::desc::{self, ClusterHandler as _};
use rs_matter::dm::clusters::groups::{self, ClusterHandler as _};
use rs_matter::dm::clusters::net_comm::NetworkType;
use rs_matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_DET};
use rs_matter::dm::devices::DEV_TYPE_ON_OFF_LIGHT;
use rs_matter::dm::endpoints::ROOT_ENDPOINT_ID;
use rs_matter::dm::networks::wireless::ThreadNetworks;
use rs_matter::dm::networks::SysNetifs;
use rs_matter::dm::{Async, Cluster, DataModel, Dataver, EmptyHandler, Endpoint, Node};
use rs_matter::error::Error;
use rs_matter::pairing::qr::{no_optional_data, CommFlowType, QrPayload};
use rs_matter::pairing::{BasicCommData, DiscoveryCapabilities};
use rs_matter::persist::DirKvBlobStore;
use rs_matter::sc::pase::{Spake2pVerifierPassword, Spake2pVerifierPasswordRef};
use rs_matter::stack::net::std::StdNetStack;
use rs_matter::stack::wireless::PreexistingWireless;
use rs_matter::stack::MatterStack;
use rs_matter::utils::futures::Coalesce;
use rs_matter::utils::init::InitMaybeUninit;
use rs_matter::utils::sync::MaybeSend;
use rs_matter::{clusters, devices, root_endpoint, with, Matter};

use rand::Rng;

use crate::bridge::Shared;
use crate::gatt::AndroidGattPeripheral;
use crate::netctl::AndroidNetCtl;

/// The bump allocator of the stack, in bytes (as the examples).
const BUMP_SIZE: usize = 64 * 1024;

type ThreadStack = MatterStack<'static, BUMP_SIZE, ThreadNetworks>;

/// What Kotlin's `MatterNode.start` passes in.
pub struct NodeConfig {
    /// Where the node keeps its fabrics, networks and attributes (the app's files dir).
    pub storage_dir: PathBuf,
    pub discriminator: u16,
    pub passcode: u32,
    pub device_name: String,
    pub serial: String,
    /// Also commissionable over the IP network while the commissioning window is open. Off by
    /// default: like a Thread device that isn't on any network yet, the node is then found and
    /// commissioned over BLE only, which is when commissioners send the network credentials.
    pub on_network_commissioning: bool,
}

/// Runs the node until it fails or Kotlin stops it. Blocks the calling thread.
pub fn run(shared: Arc<Shared>, cfg: NodeConfig) -> Result<(), Error> {
    // The stack keeps references to these for its lifetime; one node per process run, so leaking
    // them is fine.
    let dev_det: &'static _ = Box::leak(Box::new(BasicInfoConfig {
        device_name: Box::leak(cfg.device_name.into_boxed_str()),
        serial_no: Box::leak(cfg.serial.into_boxed_str()),
        ..TEST_DEV_DET
    }));
    let dev_comm = BasicCommData {
        password: Spake2pVerifierPassword::new_from_ref(Spake2pVerifierPasswordRef::new(&cfg.passcode.to_le_bytes())),
        discriminator: cfg.discriminator,
    };

    report_codes(&shared, dev_det, &dev_comm, cfg.on_network_commissioning)?;

    let mut slot: Box<MaybeUninit<ThreadStack>> = Box::new_uninit();
    let stack: &ThreadStack = slot.init_with(MatterStack::init(dev_det, dev_comm, &TEST_DEV_ATT));

    std::fs::create_dir_all(&cfg.storage_dir)?;
    let mut store = DirKvBlobStore::new(cfg.storage_dir.clone());
    let crypto = default_crypto(rand::rng(), DAC_PRIVKEY);
    let mut rand = crypto.rand()?;

    futures_lite::future::block_on(stack.startup(&crypto, &mut store))?;
    shared.event(
        "state",
        if stack.matter().has_fabrics() { "commissioned" } else { "commissionable" },
    );
    let kv = stack.kv(store);

    let on_off = on_off::OnOffHandler::new_standalone(
        Dataver::new_rand(&mut rand),
        1,
        rs_matter::persist::VENDOR_KEYS_START + 0x10,
        KotlinOnOff { shared: shared.clone() },
    );

    let wireless = PreexistingWireless::new(
        StdNetStack::new(),
        SysNetifs,
        AndroidNetCtl::new(NetworkType::Thread, shared.clone()),
        rs_matter::transport::network::mdns::builtin::BuiltinMdns::new(),
        AndroidGattPeripheral::new(shared.clone()),
    );
    let handler = data_model(stack.matter(), rand, &on_off);
    let matter = async {
        if cfg.on_network_commissioning {
            // BLE and the IP network at once: commissioners may pick either (and pick IP when they
            // can see it, sending no network credentials).
            stack.run_thread(wireless, &crypto, handler, kv, ()).await
        } else {
            // BLE while the commissioning window is open, then the IP network: the credentials
            // reach the app (AndroidNetCtl::connect) when the node goes operational.
            stack.run_thread_no_coex(wireless, &crypto, handler, kv, ()).await
        }
    };
    let matter = pin!(matter);

    let stop = async {
        let _ = shared.stop.1.recv().await;
        info!("Stop requested");
        Ok(())
    };

    futures_lite::future::block_on(rs_matter::utils::futures::select(matter, stop).coalesce())
}

/// Tells Kotlin the manual pairing code and the QR code text.
fn report_codes(shared: &Shared, dev_det: &BasicInfoConfig, comm: &BasicCommData, on_network: bool) -> Result<(), Error> {
    shared.event("pairing_code", &comm.compute_pretty_pairing_code());
    let caps = if on_network {
        DiscoveryCapabilities::BLE | DiscoveryCapabilities::IP
    } else {
        DiscoveryCapabilities::BLE
    };
    let payload = QrPayload::new(
        caps,
        CommFlowType::Standard,
        comm.clone(),
        dev_det.vid,
        dev_det.pid,
        dev_det.serial_no,
        no_optional_data as fn() -> _,
    );
    let mut buf = [0u8; 256];
    let (text, _) = payload.as_str(&mut buf)?;
    shared.event("qr_code", text);
    Ok(())
}

/// The node's endpoints: root (Thread network commissioning) and an On/Off light on endpoint 1
/// whose state is the app's ([`KotlinOnOff`]).
const NODE: Node<'static> = Node {
    endpoints: &[
        root_endpoint!(thread),
        Endpoint::new(
            1,
            devices!(DEV_TYPE_ON_OFF_LIGHT),
            clusters!(desc::DescHandler::CLUSTER, groups::GroupsHandler::CLUSTER, KotlinOnOff::CLUSTER),
        ),
    ],
};

fn data_model<'a, OH: OnOffHooks, LH: LevelControlHooks>(
    matter: &'a Matter<'a>,
    mut rand: impl Rng + Copy,
    on_off: &'a on_off::OnOffHandler<'a, OH, LH>,
) -> impl DataModel + 'a {
    (
        NODE,
        EmptyHandler
            .chain(
                |e, _| e == ROOT_ENDPOINT_ID,
                Async(ThreadStack::root_handler(matter, &(), rand)),
            )
            .chain(
                |e, c| e == 1 && c == desc::DescHandler::CLUSTER.id,
                Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()),
            )
            .chain(
                |e, c| e == 1 && c == groups::GroupsHandler::CLUSTER.id,
                Async(groups::GroupsHandler::new(Dataver::new_rand(&mut rand), matter).adapt()),
            )
            .chain(
                |e, c| e == 1 && c == KotlinOnOff::CLUSTER.id,
                Async(on_off::HandlerAdaptor(on_off)),
            ),
    )
}

/// On/Off whose state lives in the app: commands go to Kotlin `onOnOff(Boolean)`, and the app's
/// own changes (`MatterNode.setOnOff`) come back as out-of-band updates.
pub struct KotlinOnOff {
    shared: Arc<Shared>,
}

impl OnOffHooks for KotlinOnOff {
    const CLUSTER: Cluster<'static> = on_off_cluster::FULL_CLUSTER
        .with_revision(6)
        .with_features(Feature::LIGHTING.bits())
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
        self.shared.on_off(on);
    }

    async fn handle_off_with_effect(&self, _effect: EffectVariantEnum) {}

    async fn run<F: Fn(OutOfBandMessage) + MaybeSend>(&self, notify: F) {
        loop {
            match self.shared.on_off_updates.1.recv().await {
                Ok(on) => notify(OutOfBandMessage::Update(on)),
                Err(_) => core::future::pending::<()>().await,
            }
        }
    }
}
