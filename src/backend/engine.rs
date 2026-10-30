//! WinDivert network-layer capture + token-bucket traffic shaping.
//!
//! Two threads cooperate here:
//!   * [`run_engine`] receives every packet and, depending on the current
//!     capture mode, either counts it (sniff mode) or applies a shaping
//!     verdict (intercept mode): pass, drop (block), or, if a speed limit
//!     applies and there aren't enough tokens, enqueue the packet in a
//!     per-`(exe, direction)` bucket for later injection.
//!   * [`run_drainer`] wakes every few milliseconds, refills the buckets, and
//!     re-injects queued packets as tokens become available.
//!
//! Both threads share one WinDivert handle (see [`EngineHandle`]), held in
//! the shared handle slot so the engine can replace it on a mode switch.
//!
//! Capture modes (see `crate::types::CaptureMode`): while no rule is
//! effective, the handle runs in sniff mode (`RECV_ONLY | SNIFF`) — the
//! driver hands us copies while the originals keep flowing, so monitoring
//! costs no intercept/re-inject round-trip on the traffic path. As soon as
//! any rule becomes effective, the engine switches to the intercept handle
//! (same filter, default flags) and shaping applies as before. The sniff
//! path structurally cannot send: it is a separate code path with no `send`
//! call, and the handle itself would reject a send.
//!
//! Byte counting reflects shaped throughput in intercept mode: bytes are
//! only counted when a packet is actually injected (immediately by the
//! engine, or later by the drainer). Dropped/blocked packets are not counted.
//! In sniff mode, bytes are *observed* bytes (counted on receipt); they are
//! not claimed to be lossless — the driver drops copies when userspace falls
//! behind, and there is no API counter for that loss (see the 5s report and
//! the acceptance tests for how it is quantified instead).
//!
//! Exit handshake (strict order): the engine sets `engine_stopped` as its
//! very last action, after which it never receives or enqueues again; only
//! then does the drainer do its final flush. Blocked `recv` calls are woken
//! by the aggregator via the patched `WinDivert::shutdown(Recv)` (see the
//! `patches/windivert` note in `Cargo.toml`), never by a synthetic packet
//! and never by relying on process exit.

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windivert::prelude::*;
use windivert_sys::ChecksumFlags;

use super::Shared;
use super::synthetic;
use crate::types::CaptureMode;

/// Receive buffer size. WinDivert never hands us anything larger than a single
/// (possibly offloaded) IP packet; 64 KiB covers the maximum.
const RECV_BUFFER: usize = 65_535;
/// How often the drainer wakes to refill buckets and flush queues.
const DRAIN_INTERVAL: Duration = Duration::from_millis(5);
/// Per-bucket queue caps (whichever is hit first). Beyond these, packets are
/// dropped, which for TCP just applies additional back-pressure.
///
/// These caps are per bucket on purpose: one application's full queue never
/// reduces another application's quota (there is no shared packet pool to
/// hog). Fairness across simultaneously throttled applications follows from
/// this independence, and the multi-application unit test pins it down.
const MAX_QUEUE_PKTS: usize = 256;
const MAX_QUEUE_BYTES: usize = 1024 * 1024;
/// Global bound on distinct shaping buckets. Combined with the per-bucket
/// caps, a full flush is bounded: at most `MAX_BUCKETS * MAX_QUEUE_PKTS`
/// packets (64 * 256 = 16,384). 64 is far above any realistic rule count
/// (rules are set by hand, typically a handful); beyond it, a new throttled
/// executable's packets are dropped (back-pressure, counted in `dropped`)
/// rather than queued, instead of letting the bucket map grow without bound.
const MAX_BUCKETS: usize = 64;
/// How long the engine waits for the drainer to go idle before a mode
/// switch. Exceeding it aborts the switch (safe: the old handle keeps
/// working); the next rule change retries.
const SWITCH_DRAINER_WAIT: Duration = Duration::from_millis(100);
/// Poll interval while waiting for the drainer to go idle.
const DRAINER_IDLE_POLL: Duration = Duration::from_millis(1);
/// How long the drainer waits for `engine_stopped` before its final flush.
/// Exceeding it still flushes (best effort) rather than hanging the exit.
const ENGINE_STOP_WAIT: Duration = Duration::from_secs(5);
/// Poll interval while waiting for `engine_stopped`.
const ENGINE_STOP_POLL: Duration = Duration::from_millis(5);
/// How many distinct unattributed 5-tuples to log per 5s report.
const UNATTRIBUTED_SAMPLES: usize = 5;

/// The single network-layer WinDivert handle, shared by the engine (recv+send)
/// and drainer (send) threads.
///
/// A second handle is not an option: re-injected packets are re-presented to
/// network-layer handles of lower priority, and a lower-priority handle
/// silently drops them, breaking connectivity. Injecting through the same
/// handle that captured the packet skips re-capture entirely.
///
/// Safety: the WinDivert C API documents handles as thread-safe; the Rust
/// wrapper is conservative in not implementing Send/Sync.
pub struct EngineHandle(pub WinDivert<NetworkLayer>);
unsafe impl Send for EngineHandle {}
unsafe impl Sync for EngineHandle {}

/// WinDivert filter for the network-layer handle.
///
/// Loopback traffic is excluded on purpose. It is never link traffic, so there
/// is nothing to meter or shape, and re-injecting it is harmful: security
/// software such as NordVPN Threat Protection redirects browser connections to
/// a local proxy through a WFP connect-redirect, and a re-injected loopback
/// packet loses that redirect state, so the connection never completes and the
/// browser reports "connection closed" while every non-browser app still works.
const NETWORK_FILTER: &str = "!loopback";

/// Open the shared network-layer capture handle in intercept mode. Fails if
/// not elevated or if the WinDivert driver files are missing.
pub fn open() -> Result<EngineHandle, WinDivertError> {
    Ok(EngineHandle(WinDivert::network(
        NETWORK_FILTER,
        0,
        WinDivertFlags::new(),
    )?))
}

/// Open the shared network-layer capture handle in sniff mode:
/// `RECV_ONLY | SNIFF` means the driver hands us copies of packets while the
/// originals keep flowing through the stack untouched. Sending through this
/// handle fails by construction, so the sniff path structurally cannot
/// re-inject anything.
pub fn open_sniff() -> Result<EngineHandle, WinDivertError> {
    Ok(EngineHandle(WinDivert::network(
        NETWORK_FILTER,
        0,
        WinDivertFlags::new().set_recv_only().set_sniff(),
    )?))
}

/// Traffic direction, from the local host's point of view.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Dir {
    /// Inbound / download.
    Down,
    /// Outbound / upload.
    Up,
}

/// Bucket identity: one token bucket per executable per direction.
pub type BucketKey = (String, Dir);

/// A token bucket plus the queue of packets waiting on it.
pub struct ShapedBucket {
    pub tb: TokenBucket,
    pub queue: VecDeque<WinDivertPacket<'static, NetworkLayer>>,
    pub queued_bytes: usize,
}

impl ShapedBucket {
    pub fn new(rate: u64) -> Self {
        Self {
            tb: TokenBucket::new(rate),
            queue: VecDeque::new(),
            queued_bytes: 0,
        }
    }

    /// Release a bucket (used when its rule is removed): set an effectively
    /// infinite rate, so the drainer flushes every queued packet on its next
    /// pass (it refills before sending, so the huge rate takes effect
    /// immediately). The bucket itself stays (empty) so a later re-added rule
    /// reuses it via `set_rate`; queued packets are sent exactly once
    /// (popped under the buckets lock by whichever flush runs first), never
    /// dropped by the removal itself.
    pub fn release(&mut self) {
        self.tb.set_rate(u64::MAX);
    }
}

/// Whether a packet of `plen` bytes fits into `bucket`'s queue caps.
/// Pure helper so the cap logic is unit-testable without a driver handle.
fn queue_has_room(bucket: &ShapedBucket, plen: usize) -> bool {
    bucket.queue.len() < MAX_QUEUE_PKTS && bucket.queued_bytes + plen <= MAX_QUEUE_BYTES
}

/// Whether a bucket may be created for a new key when `bucket_count` buckets
/// already exist. Existing keys are always allowed (they already hold a
/// slot); only brand-new keys are subject to the global bucket cap.
fn bucket_admission_allowed(bucket_count: usize, is_new_key: bool) -> bool {
    !is_new_key || bucket_count < MAX_BUCKETS
}

/// A byte-denominated token bucket. Capacity equals `rate`, giving a one-second
/// burst allowance; tokens refill continuously at `rate` bytes/second.
#[derive(Debug)]
pub struct TokenBucket {
    rate: u64,
    capacity: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(rate: u64) -> Self {
        Self::new_at(rate, Instant::now())
    }

    /// Construct with an explicit start instant (used by tests for determinism).
    pub fn new_at(rate: u64, now: Instant) -> Self {
        let capacity = rate as f64;
        Self {
            rate,
            capacity,
            tokens: capacity,
            last: now,
        }
    }

    /// Update the fill rate (e.g. after a rule edit), clamping current tokens to
    /// the new capacity.
    pub fn set_rate(&mut self, rate: u64) {
        if rate != self.rate {
            self.rate = rate;
            self.capacity = rate as f64;
            if self.tokens > self.capacity {
                self.tokens = self.capacity;
            }
        }
    }

    /// Add tokens for elapsed time since the last refill, capped at capacity.
    pub fn refill(&mut self, now: Instant) {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        if dt > 0.0 {
            self.tokens = (self.tokens + dt * self.rate as f64).min(self.capacity);
            self.last = now;
        }
    }

    /// Try to spend `n` bytes' worth of tokens. Returns `true` if consumed.
    ///
    /// A packet larger than the whole capacity can never fit under the normal
    /// rule, so as a special case we let it through once the bucket is full
    /// (draining all tokens); otherwise such a packet would stall forever.
    pub fn try_take(&mut self, n: u64) -> bool {
        let n = n as f64;
        if n > self.capacity {
            if self.tokens >= self.capacity {
                self.tokens = 0.0;
                true
            } else {
                false
            }
        } else if self.tokens >= n {
            self.tokens -= n;
            true
        } else {
            false
        }
    }
}

/// Engine thread entry point: capture, classify, verdict, inject (or, in
/// sniff mode, count only).
///
/// The current handle is read from the shared slot once per iteration, so a
/// mode switch (which replaces the slot) takes effect at a packet boundary —
/// never in the middle of a packet's processing. The loop-top reconciliation
/// runs before every `recv`, so a rule change is picked up within one packet
/// of arriving.
pub fn run_engine(shared: Arc<Shared>) {
    tracing::info!("engine thread started");

    let mut buf = vec![0u8; RECV_BUFFER];
    let mut stats = EngineStats::default();
    let mut last_report = Instant::now();
    let mut last_switch_attempt: Option<(CaptureMode, CaptureMode)> = None;
    loop {
        // Loop-top reconciliation (only while running; on shutdown the
        // engine exits instead of switching).
        if shared.running.load(Ordering::Relaxed) {
            let desired = CaptureMode::from_u8(shared.desired_mode.load(Ordering::Relaxed));
            let actual = CaptureMode::from_u8(shared.actual_mode.load(Ordering::Relaxed));
            if desired == actual {
                // Desired matches actual again (e.g. the last effective rule
                // was deleted after a failed switch): any stale sticky error
                // is cleared, and with it the GUI banner.
                if shared.mode_error.lock().unwrap().take().is_some() {
                    tracing::info!(
                        "engine: mode reconciled (desired == actual == {:?}), clearing sticky error",
                        desired
                    );
                }
            } else if switch_attempt_due(desired, actual, &mut last_switch_attempt) {
                try_switch(&shared, desired, actual);
            }
        }

        // Read the current handle from the slot (clone for this iteration).
        // At this point the engine holds no handle: a switch can take the
        // slot's Arc and close it with unique ownership.
        let slot = shared.handle_slot.read().unwrap().clone();
        let Some((handle, _gen)) = slot else {
            // Degraded: no handle, so nothing to capture. Traffic flows
            // normally (there is nothing diverting it). On shutdown, exit;
            // while running, poll for a successful switch instead of spinning.
            if !shared.running.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        let divert = &handle.0;

        let packet = match divert.recv(Some(&mut buf)) {
            Ok(p) => p,
            Err(WinDivertError::Recv(WinDivertRecvError::NoData)) => break,
            Err(e) => {
                stats.recv_err += 1;
                if !shared.running.load(Ordering::Relaxed) {
                    // Shutdown path: the aggregator woke us via
                    // shutdown(Recv) (or the handle went away during exit).
                    // Leave without touching the handle again.
                    break;
                }
                tracing::warn!("engine: recv error: {e}");
                continue;
            }
        };
        stats.recv_ok += 1;

        let mode = CaptureMode::from_u8(shared.actual_mode.load(Ordering::Relaxed));
        if mode == CaptureMode::Sniff {
            // Sniff path: count only. The packet stays borrowed (no copy),
            // no checksum recalculation, no rules lookup, no send. The
            // originals keep flowing through the stack untouched.
            handle_packet_sniff(&shared, &packet, &mut stats);
        } else {
            // Intercept path: own the packet and recalculate checksums before
            // any re-injection. Captured packets can carry incomplete
            // checksums due to hardware offload; re-injecting them unmodified
            // makes WinDivertSend report success while the packets are
            // dropped downstream.
            let mut packet = packet.into_owned();
            if packet.recalculate_checksums(ChecksumFlags::new()).is_err() {
                stats.calc_err += 1;
            }
            handle_packet(divert, &shared, packet, &mut stats);
        }

        if last_report.elapsed() >= Duration::from_secs(5) {
            let mode = CaptureMode::from_u8(shared.actual_mode.load(Ordering::Relaxed));
            tracing::info!(
                "engine 5s: mode={} recv_ok={} recv_err={} sent={} send_err={} calc_err={} counted={} unattributed={} queued={} dropped={}",
                mode.label(),
                stats.recv_ok,
                stats.recv_err,
                stats.sent,
                stats.send_err,
                stats.calc_err,
                stats.counted,
                stats.unattributed,
                stats.queued,
                stats.dropped
            );
            for sample in &stats.unattributed_samples {
                tracing::info!("engine 5s: unattributed sample {sample}");
            }
            stats = EngineStats::default();
            last_report = Instant::now();
        }
    }
    // Last action before returning: signal the drainer that no more packets
    // will be received or enqueued. The drainer only does its final flush
    // after observing this, so no packet can land in an unflushed queue.
    shared.engine_stopped.store(true, Ordering::SeqCst);
    tracing::info!("engine thread exiting");
}

/// Rolling diagnostics for the engine loop, reported every 5 seconds.
#[derive(Default)]
struct EngineStats {
    recv_ok: u64,
    recv_err: u64,
    sent: u64,
    send_err: u64,
    calc_err: u64,
    /// Packets counted in sniff mode (observed bytes, not injected bytes).
    counted: u64,
    unattributed: u64,
    queued: u64,
    dropped: u64,
    /// Up to `UNATTRIBUTED_SAMPLES` example 5-tuples we failed to attribute,
    /// logged with the 5s report so misattribution can be diagnosed.
    unattributed_samples: Vec<String>,
}

/// Classify one captured packet and apply a shaping verdict.
fn handle_packet(
    divert: &WinDivert<NetworkLayer>,
    shared: &Shared,
    packet: WinDivertPacket<'static, NetworkLayer>,
    stats: &mut EngineStats,
) {
    let outbound = packet.address.outbound();
    let len = packet.data.len() as u64;

    // Attribute the packet to a process via the flow table.
    let parsed = parse_packet(&packet.data);
    let exe = match parsed {
        Some((proto, src, dst)) => {
            let (local, remote) = if outbound { (src, dst) } else { (dst, src) };
            shared.flows.lock().unwrap().exe_for(proto, local, remote)
        }
        None => None,
    };

    if exe.is_none() {
        note_unattributed(stats, parsed, outbound, len);
    }

    // Look up the rule (cloned so we don't hold the rules lock). The "Unknown"
    // row is never shaped: see `synthetic::is_shapable`.
    let rule = exe
        .as_ref()
        .filter(|e| synthetic::is_shapable(e))
        .and_then(|e| shared.rules.lock().unwrap().get(e).cloned());

    match rule {
        // No process match, or no rule: pass through untouched.
        None => send_and_count(
            divert,
            shared,
            &packet,
            exe.as_deref(),
            outbound,
            len,
            stats,
        ),
        // Blocked: drop everything for this process.
        Some(r) if r.blocked => {
            stats.dropped += 1;
        }
        Some(r) => {
            let limit = if outbound { r.up_limit } else { r.down_limit };
            match limit {
                // This direction is unlimited: pass through.
                None => send_and_count(
                    divert,
                    shared,
                    &packet,
                    exe.as_deref(),
                    outbound,
                    len,
                    stats,
                ),
                // An explicit zero limit means "block this direction".
                Some(0) => {
                    stats.dropped += 1;
                }
                Some(rate) => {
                    let exe = exe.expect("rule implies a resolved exe path");
                    let dir = if outbound { Dir::Up } else { Dir::Down };
                    let plen = packet.data.len();

                    let mut buckets = shared.buckets.lock().unwrap();
                    let key = (exe.clone(), dir);
                    if !bucket_admission_allowed(buckets.len(), !buckets.contains_key(&key)) {
                        // Global bucket cap reached: drop (back-pressure)
                        // rather than queueing. This only triggers with more
                        // than MAX_BUCKETS distinct throttled executables, and
                        // it never takes quota away from existing buckets.
                        stats.dropped += 1;
                        return;
                    }
                    let bucket = buckets
                        .entry(key)
                        .or_insert_with(|| ShapedBucket::new(rate));
                    bucket.tb.set_rate(rate);
                    bucket.tb.refill(Instant::now());

                    if bucket.tb.try_take(len) {
                        // Tokens available: release the lock and inject now.
                        drop(buckets);
                        send_and_count(divert, shared, &packet, Some(&exe), outbound, len, stats);
                    } else if queue_has_room(bucket, plen) {
                        // Hold the packet for the drainer to inject later.
                        bucket.queued_bytes += plen;
                        bucket.queue.push_back(packet);
                        stats.queued += 1;
                    } else {
                        // Queue full: drop (back-pressure).
                        stats.dropped += 1;
                    }
                }
            }
        }
    }
}

/// Inject a packet and, if attributed, count its bytes as shaped throughput.
fn send_and_count(
    divert: &WinDivert<NetworkLayer>,
    shared: &Shared,
    packet: &WinDivertPacket<NetworkLayer>,
    exe: Option<&str>,
    outbound: bool,
    len: u64,
    stats: &mut EngineStats,
) {
    if let Err(e) = divert.send(packet) {
        stats.send_err += 1;
        if stats.send_err <= 5 || stats.send_err.is_multiple_of(500) {
            tracing::warn!(
                "engine: send failed (#{}) len={len} outbound={outbound}: {e}",
                stats.send_err
            );
        }
        return;
    }
    stats.sent += 1;
    let (down, up) = if outbound { (0, len) } else { (len, 0) };
    let mut flows = shared.flows.lock().unwrap();
    match exe {
        Some(exe) => flows.add_bytes(exe, down, up),
        // Unattributed traffic still counts, under the "Unknown" row, so the
        // totals and the graph reflect actual link usage.
        None => flows.add_unattributed(down, up),
    }
}

/// Record an unattributed packet in the 5s diagnostics. Shared by the sniff
/// and intercept paths so Unknown reporting stays identical in both modes.
fn note_unattributed(
    stats: &mut EngineStats,
    parsed: Option<(u8, SocketAddr, SocketAddr)>,
    outbound: bool,
    len: u64,
) {
    stats.unattributed += 1;
    if stats.unattributed_samples.len() < UNATTRIBUTED_SAMPLES {
        let desc = match parsed {
            Some((proto, src, dst)) => {
                format!("proto={proto} src={src} dst={dst} outbound={outbound} len={len}")
            }
            None => format!("unparsed len={len} outbound={outbound}"),
        };
        if !stats.unattributed_samples.contains(&desc) {
            stats.unattributed_samples.push(desc);
        }
    }
}

/// Sniff-mode packet handling: attribute and count only.
///
/// Structural guarantees: this function never calls `send`, never consults
/// the rules lock (the mode itself already encodes "no effective rule"), and
/// never copies the packet (it works on the borrowed buffer). Attribution
/// uses the same flow table and the same Unknown row as the intercept path,
/// so per-process totals and the Unknown row stay comparable across modes.
///
/// Honest accounting: the bytes counted here are *observed* bytes. If
/// userspace falls behind, the driver drops copies (the real traffic is
/// unaffected), and no API counter reports that loss — so sniff totals are
/// an observation with a tolerance (see the acceptance tests), not a
/// lossless claim.
fn handle_packet_sniff(
    shared: &Shared,
    packet: &WinDivertPacket<NetworkLayer>,
    stats: &mut EngineStats,
) {
    let outbound = packet.address.outbound();
    let len = packet.data.len() as u64;
    let parsed = parse_packet(&packet.data);
    let (down, up) = if outbound { (0, len) } else { (len, 0) };

    // One lock for attribution + counting (`exe_for` takes &self, the `add_*`
    // calls take &mut self, both through the same guard).
    let mut flows = shared.flows.lock().unwrap();
    let exe = parsed.and_then(|(proto, src, dst)| {
        let (local, remote) = if outbound { (src, dst) } else { (dst, src) };
        flows.exe_for(proto, local, remote)
    });
    match exe {
        Some(exe) => flows.add_bytes(&exe, down, up),
        None => {
            note_unattributed(stats, parsed, outbound, len);
            flows.add_unattributed(down, up);
        }
    }
    stats.counted += 1;
}

/// Record a sticky mode error (engine-only writer). The aggregator copies it
/// into snapshots for the GUI banner; it is cleared only on a successful
/// switch or when the desired mode reconciles with the actual mode.
fn set_mode_error(shared: &Shared, msg: String) {
    tracing::error!("mode error: {msg}");
    *shared.mode_error.lock().unwrap() = Some(msg);
}

/// Pure switch-retry guard: a switch is attempted only when the
/// (desired, actual) pair differs from the last attempted pair. Without this,
/// a persistently failing switch would be retried on every packet under load
/// (each attempt costs a drainer handshake). A rule change produces a new
/// pair and retries; a successful switch makes desired == actual, which the
/// reconciliation branch handles instead.
fn switch_attempt_due(
    desired: CaptureMode,
    actual: CaptureMode,
    last_attempt: &mut Option<(CaptureMode, CaptureMode)>,
) -> bool {
    let pair = (desired, actual);
    if *last_attempt == Some(pair) {
        false
    } else {
        *last_attempt = Some(pair);
        true
    }
}

/// Pop every queued packet (all buckets) and inject it through the current
/// slot handle, counting each successful injection exactly once. Used before
/// a mode switch (so no queued packet is stranded on a retiring handle) and
/// by the drainer's exit flush. Returns `(sent, send_failed)`; failed sends
/// are already popped, so they are lost (logged here) — this only happens on
/// a driver error, not in normal operation.
///
/// No-op when the slot holds no handle (Degraded) or all queues are empty.
fn flush_all_buckets(shared: &Shared) -> (usize, usize) {
    let slot = shared.handle_slot.read().unwrap().clone();
    let Some((handle, _gen)) = slot else {
        return (0, 0);
    };
    let divert = &handle.0;

    let mut items: Vec<(WinDivertPacket<'static, NetworkLayer>, String, u64, u64)> = Vec::new();
    {
        let mut buckets = shared.buckets.lock().unwrap();
        for ((exe, dir), bucket) in buckets.iter_mut() {
            while let Some(pkt) = bucket.queue.pop_front() {
                let plen = pkt.data.len();
                bucket.queued_bytes = bucket.queued_bytes.saturating_sub(plen);
                let (down, up) = match dir {
                    Dir::Down => (plen as u64, 0),
                    Dir::Up => (0, plen as u64),
                };
                items.push((pkt, exe.clone(), down, up));
            }
        }
    }

    let mut sent = 0;
    let mut failed = 0;
    let mut counted: Vec<(String, u64, u64)> = Vec::new();
    for (pkt, exe, down, up) in &items {
        match divert.send(pkt) {
            Ok(_) => {
                sent += 1;
                counted.push((exe.clone(), *down, *up));
            }
            Err(e) => {
                failed += 1;
                tracing::warn!("flush: send failed: {e}");
            }
        }
    }
    if !counted.is_empty() {
        let mut flows = shared.flows.lock().unwrap();
        for (exe, down, up) in counted {
            flows.add_bytes(&exe, down, up);
        }
    }
    (sent, failed)
}

/// Attempt a capture-mode switch (engine thread only, at a packet boundary).
///
/// Order of operations (each step checks `running` first; on shutdown the
/// switch aborts and the engine exits instead):
/// 1. Pause the drainer (set `switching`, wait for `drainer_idle`, bounded).
///    While paused, the drainer holds no handle clone and sends nothing.
/// 2. Flush all queued packets through the OLD handle (still in the slot),
///    so no queued packet is stranded on a retiring handle.
/// 3. Take the slot and close the old handle (unique ownership via
///    `try_unwrap`). On close failure, restore the slot and abort — the old
///    mode keeps working, and the failure is reported, not hidden.
/// 4. Open the new handle. On failure, fall back: reopen the OLD mode and
///    report that the new rule is not enforced; if that also fails, enter
///    Degraded (no handle) and report that neither capture nor shaping works.
/// 5. Publish the new handle with a fresh generation, set `actual_mode`,
///    clear the sticky error on success, and resume the drainer.
///
/// Failure accounting: a failed switch never leaves the UI believing a rule
/// is enforced when it is not — every failure path writes a sticky
/// `mode_error` (shown as a banner) except a shutdown abort, where the
/// process is exiting anyway.
///
/// Known, documented gaps (not hidden): between closing the old handle and
/// opening the new one there is a sub-second window with no handle, during
/// which packets flow normally, uncounted and (for a new rule) unshaped;
/// and packets still sitting unread in the OLD handle's driver queue are
/// discarded when it closes (intercept -> sniff direction only — a sniff
/// handle's queue holds copies, so closing it loses no real traffic).
fn try_switch(shared: &Shared, desired: CaptureMode, actual: CaptureMode) {
    if desired == CaptureMode::Degraded {
        // Defensive: desired comes from the rule set and is never Degraded.
        return;
    }
    tracing::info!(
        "engine: switching capture mode {} -> {}",
        actual.label(),
        desired.label()
    );

    // Step 1: pause the drainer.
    shared.switching.store(true, Ordering::SeqCst);
    let mut waited = Duration::ZERO;
    while !shared.drainer_idle.load(Ordering::SeqCst) {
        if !shared.running.load(Ordering::Relaxed) {
            // Shutdown during the handshake: resume the drainer (it will
            // proceed to its exit flush) and leave without an error.
            shared.switching.store(false, Ordering::SeqCst);
            return;
        }
        if waited >= SWITCH_DRAINER_WAIT {
            set_mode_error(
                shared,
                format!(
                    "切换到{}失败：排空线程未能暂停，仍保持{}模式",
                    desired.label(),
                    actual.label()
                ),
            );
            shared.switching.store(false, Ordering::SeqCst);
            return;
        }
        std::thread::sleep(DRAINER_IDLE_POLL);
        waited += DRAINER_IDLE_POLL;
    }

    // Step 2: flush queues through the old handle.
    if !shared.running.load(Ordering::Relaxed) {
        shared.switching.store(false, Ordering::SeqCst);
        return;
    }
    let (sent, failed) = flush_all_buckets(shared);
    if sent + failed > 0 {
        tracing::info!("engine: switch flush sent={sent} failed={failed}");
    }

    // Step 3: take the slot and close the old handle (if any).
    if !shared.running.load(Ordering::Relaxed) {
        shared.switching.store(false, Ordering::SeqCst);
        return;
    }
    let old = shared.handle_slot.write().unwrap().take();
    if let Some((old_handle, old_gen)) = old {
        match Arc::try_unwrap(old_handle) {
            Ok(mut engine_handle) => {
                if let Err(e) = engine_handle.0.close(CloseAction::Nothing) {
                    // Close failed: the handle is still open and we still own
                    // it, so restore the slot and abort — the old mode keeps
                    // working, and the failure is reported.
                    tracing::error!("engine: switch aborted: old handle close failed: {e}");
                    *shared.handle_slot.write().unwrap() = Some((Arc::new(engine_handle), old_gen));
                    set_mode_error(
                        shared,
                        format!(
                            "切换到{}失败：旧句柄关闭失败，仍保持{}模式",
                            desired.label(),
                            actual.label()
                        ),
                    );
                    shared.switching.store(false, Ordering::SeqCst);
                    return;
                }
                // Close succeeded: the old handle is retired. Packets still
                // unread in its driver queue are discarded (see fn docs).
            }
            Err(arc) => {
                // Unexpected holder (the drainer is idle and the engine holds
                // no clone at loop-top, so this should not happen). Restore
                // and abort rather than forcing a second handle.
                tracing::error!("engine: switch aborted: old handle still shared");
                *shared.handle_slot.write().unwrap() = Some((arc, old_gen));
                set_mode_error(
                    shared,
                    format!(
                        "切换到{}失败：旧句柄仍被占用，仍保持{}模式",
                        desired.label(),
                        actual.label()
                    ),
                );
                shared.switching.store(false, Ordering::SeqCst);
                return;
            }
        }
    }

    // Step 4: open the new handle (shutdown aborts leave the slot empty;
    // the engine exits and the drainer finds nothing to flush, which is
    // correct because step 2 already flushed the queues).
    if !shared.running.load(Ordering::Relaxed) {
        shared
            .actual_mode
            .store(CaptureMode::Degraded.to_u8(), Ordering::Relaxed);
        shared.switching.store(false, Ordering::SeqCst);
        return;
    }
    let opened = match desired {
        CaptureMode::Sniff => open_sniff(),
        CaptureMode::Divert => open(),
        CaptureMode::Degraded => unreachable!(),
    };
    match opened {
        Ok(new_handle) => {
            let generation = shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
            *shared.handle_slot.write().unwrap() = Some((Arc::new(new_handle), generation));
            shared.actual_mode.store(desired.to_u8(), Ordering::Relaxed);
            shared.mode_error.lock().unwrap().take();
            tracing::info!(
                "engine: switched capture mode {} -> {} (gen {generation})",
                actual.label(),
                desired.label()
            );
        }
        Err(e) => {
            tracing::error!(
                "engine: failed to open {} handle: {e}; attempting fallback to {}",
                desired.label(),
                actual.label()
            );
            // Fallback: reopen the OLD mode (only if there was one).
            let fallback = match actual {
                CaptureMode::Sniff => Some(open_sniff()),
                CaptureMode::Divert => Some(open()),
                CaptureMode::Degraded => None,
            };
            match fallback {
                Some(Ok(fb_handle)) => {
                    let generation = shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
                    *shared.handle_slot.write().unwrap() = Some((Arc::new(fb_handle), generation));
                    shared.actual_mode.store(actual.to_u8(), Ordering::Relaxed);
                    let msg = match (desired, actual) {
                        (CaptureMode::Divert, CaptureMode::Sniff) => {
                            "切换到拦截失败，已恢复监控：新规则的限速未生效".to_string()
                        }
                        (CaptureMode::Sniff, CaptureMode::Divert) => {
                            "切换到监控失败，已恢复拦截：流量仍走拦截路径（速度可能受影响），规则本身已按最新状态执行"
                                .to_string()
                        }
                        _ => format!(
                            "切换到{}失败，已恢复{}模式",
                            desired.label(),
                            actual.label()
                        ),
                    };
                    set_mode_error(shared, msg);
                }
                _ => {
                    *shared.handle_slot.write().unwrap() = None;
                    shared
                        .actual_mode
                        .store(CaptureMode::Degraded.to_u8(), Ordering::Relaxed);
                    set_mode_error(
                        shared,
                        "拦截与监控句柄均打开失败，当前无捕获：限速与统计均未生效".to_string(),
                    );
                }
            }
        }
    }

    // Step 5: resume the drainer.
    shared.switching.store(false, Ordering::SeqCst);
}

/// Drainer thread entry point: flush queued packets as tokens refill.
///
/// Re-injects through the same handle the engine captures on; see
/// [`EngineHandle`] for why a second handle is not an option.
pub fn run_drainer(shared: Arc<Shared>) {
    tracing::info!("drainer thread started");

    while shared.running.load(Ordering::Relaxed) {
        // Mode-switch pause: when the engine is switching handles, signal
        // idle, wait until it clears the flag, then resume. While idle, this
        // thread holds no handle clone and sends nothing, which is what lets
        // the engine close the old handle with unique ownership.
        if shared.switching.load(Ordering::SeqCst) {
            shared.drainer_idle.store(true, Ordering::SeqCst);
            while shared.switching.load(Ordering::SeqCst) && shared.running.load(Ordering::Relaxed)
            {
                std::thread::sleep(DRAINER_IDLE_POLL);
            }
            shared.drainer_idle.store(false, Ordering::SeqCst);
            if !shared.running.load(Ordering::Relaxed) {
                break;
            }
            continue;
        }

        // Read the current handle from the slot once per iteration, so a
        // switch takes effect at an iteration boundary.
        let slot = shared.handle_slot.read().unwrap().clone();
        let Some((handle, _gen)) = slot else {
            // Degraded: no handle, nothing to send. Poll (do not spin).
            std::thread::sleep(DRAIN_INTERVAL);
            continue;
        };
        let mode = CaptureMode::from_u8(shared.actual_mode.load(Ordering::Relaxed));
        if mode == CaptureMode::Sniff {
            // Sniff mode never queues (the sniff path has no queueing branch)
            // and a sniff handle rejects sends, so there is nothing to do.
            // A non-empty queue here would mean a switch bug; report it and
            // leave the packets alone (never send via a sniff handle, never
            // drop silently).
            let non_empty = shared
                .buckets
                .lock()
                .unwrap()
                .values()
                .any(|b| !b.queue.is_empty());
            if non_empty {
                tracing::error!(
                    "drainer: unexpected queued packets in sniff mode; leaving them untouched"
                );
            }
            std::thread::sleep(DRAIN_INTERVAL);
            continue;
        }
        let divert = &handle.0;

        let now = Instant::now();
        // Collected under the buckets lock, executed after releasing it so we
        // never hold two locks at once. The engine takes flows then buckets;
        // keeping the drainer to one lock at a time avoids any ordering hazard.
        let mut to_send: Vec<WinDivertPacket<'static, NetworkLayer>> = Vec::new();
        let mut to_count: Vec<(String, u64, u64)> = Vec::new();

        {
            let mut buckets = shared.buckets.lock().unwrap();
            for ((exe, dir), bucket) in buckets.iter_mut() {
                bucket.tb.refill(now);
                while let Some(front) = bucket.queue.front() {
                    let plen = front.data.len();
                    if bucket.tb.try_take(plen as u64) {
                        let pkt = bucket.queue.pop_front().unwrap();
                        bucket.queued_bytes = bucket.queued_bytes.saturating_sub(plen);
                        let (down, up) = match dir {
                            Dir::Down => (plen as u64, 0),
                            Dir::Up => (0, plen as u64),
                        };
                        to_count.push((exe.clone(), down, up));
                        to_send.push(pkt);
                    } else {
                        break;
                    }
                }
            }
        }

        for pkt in &to_send {
            if let Err(e) = divert.send(pkt) {
                tracing::warn!("drainer: send failed: {e}");
            }
        }
        if !to_count.is_empty() {
            let mut flows = shared.flows.lock().unwrap();
            for (exe, down, up) in to_count {
                flows.add_bytes(&exe, down, up);
            }
        }

        std::thread::sleep(DRAIN_INTERVAL);
    }

    // Exit path: the engine has stopped receiving and enqueueing only after
    // it sets `engine_stopped` (its last action). Wait for that signal
    // (bounded), then do one final flush through the current slot handle.
    // If the engine never signals (it died or is stuck), still flush once
    // (best effort) rather than hanging the exit; the aggregator's bounded
    // join is the outer guard against a stuck thread.
    let mut waited = Duration::ZERO;
    while !shared.engine_stopped.load(Ordering::SeqCst) && waited < ENGINE_STOP_WAIT {
        std::thread::sleep(ENGINE_STOP_POLL);
        waited += ENGINE_STOP_POLL;
    }
    if !shared.engine_stopped.load(Ordering::SeqCst) {
        tracing::warn!(
            "drainer: engine_stopped not observed within {ENGINE_STOP_WAIT:?}; flushing anyway (best effort)"
        );
    }
    let (sent, failed) = flush_all_buckets(&shared);
    if sent + failed > 0 {
        tracing::info!("drainer: exit flush sent={sent} failed={failed}");
    }
    tracing::info!("drainer thread exiting");
}

/// Zero-copy parse of an IPv4/IPv6 packet into `(protocol, src, dst)` socket
/// addresses. Ports are extracted for TCP/UDP; other protocols yield port 0
/// (which won't match a flow-table entry). Returns `None` for truncated or
/// non-IP data.
fn parse_packet(data: &[u8]) -> Option<(u8, SocketAddr, SocketAddr)> {
    let version = data.first()? >> 4;
    match version {
        4 => {
            if data.len() < 20 {
                return None;
            }
            let ihl = (data[0] & 0x0f) as usize * 4;
            if ihl < 20 || data.len() < ihl {
                return None;
            }
            let proto = data[9];
            let src_ip = Ipv4Addr::new(data[12], data[13], data[14], data[15]);
            let dst_ip = Ipv4Addr::new(data[16], data[17], data[18], data[19]);
            let (sp, dp) = l4_ports(proto, &data[ihl..]);
            Some((
                proto,
                SocketAddr::new(IpAddr::V4(src_ip), sp),
                SocketAddr::new(IpAddr::V4(dst_ip), dp),
            ))
        }
        6 => {
            if data.len() < 40 {
                return None;
            }
            // IPv6 extension headers are not walked; `next_header` is treated as
            // the L4 protocol. Non-TCP/UDP yields port 0.
            let proto = data[6];
            let src_ip = Ipv6Addr::from(<[u8; 16]>::try_from(&data[8..24]).ok()?);
            let dst_ip = Ipv6Addr::from(<[u8; 16]>::try_from(&data[24..40]).ok()?);
            let (sp, dp) = l4_ports(proto, &data[40..]);
            Some((
                proto,
                SocketAddr::new(IpAddr::V6(src_ip), sp),
                SocketAddr::new(IpAddr::V6(dst_ip), dp),
            ))
        }
        _ => None,
    }
}

/// Extract `(src_port, dst_port)` for TCP (6) / UDP (17); `(0, 0)` otherwise.
fn l4_ports(proto: u8, l4: &[u8]) -> (u16, u16) {
    if (proto == 6 || proto == 17) && l4.len() >= 4 {
        (
            u16::from_be_bytes([l4[0], l4[1]]),
            u16::from_be_bytes([l4[2], l4[3]]),
        )
    } else {
        (0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_bucket_allows_burst_up_to_capacity() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new_at(1000, t0);
        // Starts full: can spend the whole capacity at once.
        assert!(b.try_take(1000));
        // Now empty: even one byte fails.
        assert!(!b.try_take(1));
    }

    #[test]
    fn refill_is_proportional_to_elapsed_time() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new_at(1000, t0);
        assert!(b.try_take(1000)); // drain
        // After 0.5s, ~500 tokens are back.
        b.refill(t0 + Duration::from_millis(500));
        assert!(b.try_take(500));
        assert!(!b.try_take(1));
    }

    #[test]
    fn refill_never_exceeds_capacity() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new_at(1000, t0);
        assert!(b.try_take(1000));
        // Idle for 10s, but capacity caps the burst at 1000.
        b.refill(t0 + Duration::from_secs(10));
        assert!(b.try_take(1000));
        assert!(!b.try_take(1));
    }

    #[test]
    fn oversized_packet_passes_only_when_full() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new_at(500, t0); // capacity 500
        // A 1500-byte packet is larger than capacity: passes while full.
        assert!(b.try_take(1500));
        // Bucket now drained; the next oversized packet must wait.
        assert!(!b.try_take(1500));
        // After a full second it's full again and passes.
        b.refill(t0 + Duration::from_secs(1));
        assert!(b.try_take(1500));
    }

    #[test]
    fn set_rate_clamps_tokens_to_new_capacity() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new_at(1000, t0); // full: 1000 tokens
        b.set_rate(400); // capacity shrinks; tokens clamp to 400
        assert!(b.try_take(400));
        assert!(!b.try_take(1));
    }

    #[test]
    fn parses_ipv4_tcp_5tuple() {
        // Minimal IPv4 header (IHL=5 => 20 bytes), protocol=TCP(6), then ports.
        let mut pkt = vec![0u8; 24];
        pkt[0] = 0x45; // version 4, IHL 5
        pkt[9] = 6; // TCP
        pkt[12..16].copy_from_slice(&[10, 0, 0, 1]); // src
        pkt[16..20].copy_from_slice(&[93, 184, 216, 34]); // dst
        pkt[20..22].copy_from_slice(&50000u16.to_be_bytes()); // src port
        pkt[22..24].copy_from_slice(&443u16.to_be_bytes()); // dst port

        let (proto, src, dst) = parse_packet(&pkt).unwrap();
        assert_eq!(proto, 6);
        assert_eq!(src, "10.0.0.1:50000".parse().unwrap());
        assert_eq!(dst, "93.184.216.34:443".parse().unwrap());
    }

    #[test]
    fn rejects_truncated_and_non_ip() {
        assert!(parse_packet(&[]).is_none());
        assert!(parse_packet(&[0x45, 0x00]).is_none()); // too short for v4
        assert!(parse_packet(&[0x00; 20]).is_none()); // version 0
    }

    #[test]
    fn release_rate_flushes_everything_on_next_refill() {
        let mut bucket = ShapedBucket::new(100);
        assert!(bucket.tb.try_take(100)); // drain
        assert!(!bucket.tb.try_take(1));
        bucket.release();
        // The drainer refills before sending; after a refill the huge rate
        // makes any packet fit.
        bucket.tb.refill(Instant::now() + Duration::from_millis(10));
        assert!(bucket.tb.try_take(1_000_000_000));
    }

    #[test]
    fn bucket_admission_caps_new_keys_only() {
        assert!(bucket_admission_allowed(MAX_BUCKETS - 1, true));
        // At the cap, new keys are refused (back-pressure, not growth).
        assert!(!bucket_admission_allowed(MAX_BUCKETS, true));
        assert!(!bucket_admission_allowed(MAX_BUCKETS + 100, true));
        // Existing keys are never refused, even at the cap: their quota is
        // theirs, independent of how many other buckets exist.
        assert!(bucket_admission_allowed(MAX_BUCKETS, false));
        assert!(bucket_admission_allowed(MAX_BUCKETS + 100, false));
    }

    #[test]
    fn queue_caps_are_per_bucket_not_shared() {
        // Multi-application fairness, packet-count half: fill application A's
        // queue to its packet cap; B and C must still have room in theirs.
        // (Test packets are constructed, never sent; only length/count matter.)
        let mut a = ShapedBucket::new(1024);
        for _ in 0..MAX_QUEUE_PKTS {
            // Fully qualified: `WinDivertPacket::new` exists for both the
            // network and forward layers, so the bare path is ambiguous.
            let pkt = unsafe { <WinDivertPacket<'static, NetworkLayer>>::new(vec![0u8; 100]) };
            a.queue.push_back(pkt);
        }
        a.queued_bytes = MAX_QUEUE_PKTS * 100;
        assert!(!queue_has_room(&a, 100));
        let b = ShapedBucket::new(1024);
        let c = ShapedBucket::new(1024);
        assert!(queue_has_room(&b, 1500));
        assert!(queue_has_room(&c, 1500));
        // Byte-cap half: A at its byte cap, B/C unaffected.
        let mut a2 = ShapedBucket::new(1024);
        a2.queued_bytes = MAX_QUEUE_BYTES;
        assert!(!queue_has_room(&a2, 1));
        assert!(queue_has_room(&b, 1));
    }

    #[test]
    fn switch_retry_guard_only_retries_on_new_pair() {
        let mut last = None;
        // First attempt for a pair is due.
        assert!(switch_attempt_due(
            CaptureMode::Divert,
            CaptureMode::Sniff,
            &mut last
        ));
        // Same pair again: not due (no hot retry loop under load).
        assert!(!switch_attempt_due(
            CaptureMode::Divert,
            CaptureMode::Sniff,
            &mut last
        ));
        // A rule change produces a new pair: due again.
        assert!(switch_attempt_due(
            CaptureMode::Sniff,
            CaptureMode::Divert,
            &mut last
        ));
        assert!(!switch_attempt_due(
            CaptureMode::Sniff,
            CaptureMode::Divert,
            &mut last
        ));
    }
}
