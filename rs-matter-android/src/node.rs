//! The node: an rs-matter stack commissioned over BLE (Android's GATT server) and run over
//! Android's IP network, with the credentials it receives handed to Kotlin, and the app's
//! endpoints ([`crate::endpoints`]).

use core::mem::MaybeUninit;
use core::pin::pin;
use std::path::PathBuf;
use std::sync::Arc;

use log::{info, warn};

use rs_matter::crypto::{default_crypto, Crypto};
use rs_matter::dm::clusters::basic_info::BasicInfoConfig;
use rs_matter::dm::clusters::desc::{self, ClusterHandler as _};
use rs_matter::dm::clusters::groups::{self, ClusterHandler as _};
use rs_matter::dm::clusters::net_comm::NetworkType;
use rs_matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_DET};
use rs_matter::dm::endpoints::ROOT_ENDPOINT_ID;
use rs_matter::dm::networks::wireless::ThreadNetworks;
use rs_matter::dm::networks::SysNetifs;
use rs_matter::dm::{Async, DataModel, Dataver, EmptyHandler, EndptId, Endpoint, Node};
use rs_matter::error::Error;
use rs_matter::pairing::qr::{no_optional_data, CommFlowType, QrPayload};
use rs_matter::pairing::{BasicCommData, DiscoveryCapabilities};
use rs_matter::persist::DirKvBlobStore;
use rs_matter::sc::pase::{Spake2pVerifierPassword, Spake2pVerifierPasswordRef};
use rs_matter::stack::net::std::StdNetStack;
use rs_matter::stack::wireless::PreexistingWireless;
use rs_matter::stack::MatterStack;
use rs_matter::utils::futures::{select3, Coalesce};
use rs_matter::utils::init::InitMaybeUninit;
use rs_matter::{root_endpoint, Matter};

use rand::Rng;

use crate::bridge::Shared;
use crate::endpoints::{self, AppEndpoints, Kind};
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
    /// The app's endpoints (1 and up), in order.
    pub endpoints: Vec<(EndptId, Kind)>,
}

/// What Kotlin can ask a running node to do.
pub enum Command {
    /// Open the basic commissioning window (15 minutes): pairing again, or with another controller.
    OpenCommissioningWindow,
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
    let caps = if cfg.on_network_commissioning {
        DiscoveryCapabilities::BLE | DiscoveryCapabilities::IP
    } else {
        DiscoveryCapabilities::BLE
    };

    report_codes(&shared, dev_det, &dev_comm, caps)?;

    let node: Node<'static> = node(&cfg.endpoints);
    endpoints::log(&cfg.endpoints);

    let mut slot: Box<MaybeUninit<ThreadStack>> = Box::new_uninit();
    let stack: &ThreadStack = slot.init_with(MatterStack::init(dev_det, dev_comm.clone(), &TEST_DEV_ATT));

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

    let app = AppEndpoints::new(
        &cfg.endpoints,
        shared.clone(),
        shared.values.1.clone(),
        &mut rand,
        rs_matter::persist::VENDOR_KEYS_START + 0x10,
    );

    let wireless = PreexistingWireless::new(
        StdNetStack::new(),
        SysNetifs,
        AndroidNetCtl::new(NetworkType::Thread, shared.clone()),
        rs_matter::transport::network::mdns::builtin::BuiltinMdns::new(),
        AndroidGattPeripheral::new(shared.clone()),
    );
    let handler = data_model(node, stack.matter(), rand, &app);
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

    let commands = async {
        while let Ok(cmd) = shared.commands.1.recv().await {
            match cmd {
                Command::OpenCommissioningWindow => {
                    match stack.open_basic_comm_window(&crypto, &()) {
                        Ok(()) => {
                            info!("Commissioning window opened");
                            let _ = report_codes(&shared, dev_det, &dev_comm, caps);
                            shared.event("state", "commissionable");
                        }
                        Err(e) => {
                            warn!("Can't open the commissioning window: {e:?}");
                            shared.event("window_error", &format!("{e:?}"));
                        }
                    }
                }
            }
        }
        core::future::pending::<Result<(), Error>>().await
    };

    let stop = async {
        let _ = shared.stop.1.recv().await;
        info!("Stop requested");
        Ok(())
    };

    // A local, so the futures (which borrow the stack, the crypto and the endpoints) end first.
    let result = futures_lite::future::block_on(select3(pin!(matter), pin!(commands), pin!(stop)).coalesce());
    result
}

/// The root endpoint (Thread network commissioning), then the app's endpoints.
fn node(eps: &[(EndptId, Kind)]) -> Node<'static> {
    const ROOT: Endpoint<'static> = root_endpoint!(thread);
    let mut all: Vec<Endpoint<'static>> = vec![ROOT];
    all.extend(endpoints::metadata(eps));
    Node { endpoints: Box::leak(all.into_boxed_slice()) }
}

/// Tells Kotlin the manual pairing code and the QR code text.
fn report_codes(shared: &Shared, dev_det: &BasicInfoConfig, comm: &BasicCommData, caps: DiscoveryCapabilities) -> Result<(), Error> {
    shared.event("pairing_code", &comm.compute_pretty_pairing_code());
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

/// The root endpoint's handlers, the Descriptor and Groups clusters of the app endpoints, and the
/// app endpoints' own clusters ([`AppEndpoints`]).
fn data_model<'a>(
    node: Node<'static>,
    matter: &'a Matter<'a>,
    mut rand: impl Rng + Copy,
    app: &'a AppEndpoints<'a>,
) -> impl DataModel + 'a {
    (
        node,
        EmptyHandler
            .chain(
                |e, _| e == ROOT_ENDPOINT_ID,
                Async(ThreadStack::root_handler(matter, &(), rand)),
            )
            .chain(
                |e, c| e != ROOT_ENDPOINT_ID && c == desc::DescHandler::CLUSTER.id,
                Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()),
            )
            .chain(
                |e, c| e != ROOT_ENDPOINT_ID && c == groups::GroupsHandler::CLUSTER.id,
                Async(groups::GroupsHandler::new(Dataver::new_rand(&mut rand), matter).adapt()),
            )
            .chain(|e, _| e != ROOT_ENDPOINT_ID, Async(app)),
    )
}

