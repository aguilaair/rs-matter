# Upstreaming: what this fork changes in rs-matter

Draft notes for proposing this work to project-chip/rs-matter. Nothing here has been posted. A PR
there needs the author to have signed the Matter Working Group CLA (see the repo's CONTRIBUTING.md).

## Changes outside `rs-matter-android/`

### 1. Answer `ScanNetworks` during BLE commissioning (`rs-matter-stack/src/wireless.rs`)

**Problem.** During the BLE phase (non-concurrent, and the BLE part of concurrent commissioning) the
Network Commissioning cluster uses `WirelessNetCtl::Commissioning`, which answers `scan` with
`NetCtlError::Other(InvalidAction)`. `NetworkCommissioningStatusEnum::map_ctl` passes `Other` through, so
`ScanNetworks` fails with the IM status `InvalidAction`. Apple's commissioner sends `ScanNetworks` right
after `AddNOC` when commissioning a Thread device over BLE, and aborts on that error. With an empty,
successful scan it waits for the fail-safe to expire instead: it looks for the network it means to
provision in the results before sending `AddOrUpdateThreadNetwork`.

Observed with iOS (Apple Home through the Home Assistant app), on rs-matter `c800469`:
`0/0x31/0x0` (ScanNetworks) → `InvalidAction` → pairing failed; empty result → stall; one result with
the home's network (channel and PAN ID 0 were fine) → `AddOrUpdateThreadNetwork`, `ConnectNetwork`,
`CommissioningComplete`.

**This fork's change.** A process-wide `CommissioningScanner` hook (a `fn` pointer in an `AtomicPtr`,
`set_commissioning_scanner`), consulted by `WirelessNetCtl::Commissioning::scan`; without one, the scan is
empty and successful instead of failing.

**What would fit upstream better.** Something without a global:
- Give the BLE phase access to a scanner from the wireless driver: e.g. `GattTask::run` (or the
  `MatterStackWirelessTask` built for the BLE phase) gets an optional `&dyn`/generic scanner supplied by
  the `GattDriver` impl (`PreexistingWireless` has the operational `NetCtl` at hand), and
  `WirelessNetCtl::Commissioning` carries it.
- Or follow the C++ SDK for non-concurrent devices: scan once before BLE starts and answer `ScanNetworks`
  from that cache (`CHIP_DEVICE_CONFIG_SUPPORTS_CONCURRENT_CONNECTION=0` devices do this).
- Independently: an empty successful scan is a better default than `InvalidAction` (spec: `ScanNetworks`
  reports `NetworkingStatus`; a device that can't scan should still answer).

### 2. Log the invoked command path (`rs-matter-im/src/invoker.rs`)

`debug!` for each invoked command (`endpoint/cluster/command`), and the path in the existing
`Error invoking command` error. Small, independent; it is what found problem 1.

## The Android crate itself

`rs-matter-android` (JNI + a Kotlin library) is a platform port, like `rs-matter-embassy` and
`esp-idf-matter`, which live in their own repositories. It probably belongs in its own repository too,
depending on a released rs-matter that has change 1 in some form. Its `GattPeripheral` follows the Win32
one closely and could become `rs-matter-transport`'s `btp/gatt/android.rs` with the JNI side in a
companion crate, if the maintainers want Android in-tree.

## Not changed, but seen

- `RevokeCommissioning` with no window open answers `Failure` with the cluster status `WindowNotOpen`
  (spec-conformant); Apple's commissioner sends it after commissioning and goes on.
- `rs-matter-dm`'s `WirelessMgr` reconnects in a loop if a `NetCtl`'s `connected()` stays `false` after a
  successful `connect` (`wait_while_connected_status` returns at once). A `NetCtl` for a device that is
  "connected" by other means has to report `true`; worth a doc note on `WirelessDiag::connected`.
