//! Backend: capture, per-process attribution, shaping, and the 1 Hz snapshot
//! feed to the GUI.
//!
//! [`start`] spins up four named threads that communicate through an
//! `Arc<Shared>`:
//!   1. flow-events: WinDivert flow-layer events (PID-attributed) keep the
//!      flow table current; also bootstraps existing TCP connections.
//!   2. engine: WinDivert network-layer capture plus token-bucket shaping.
//!   3. drainer: re-injects packets held by the shaping buckets.
//!   4. aggregator: ticks once per second to build a [`Snapshot`] from
//!      shared state, and applies incoming [`Command`]s (rule edits, shutdown).
//!
//! Shutdown: a [`Command::Shutdown`] (or a disconnected command channel) clears
//! the `running` flag, wakes the capture threads' blocked `recv` calls via
//! the patched `WinDivert::shutdown(Recv)` (see the `patches/windivert` note
//! in `Cargo.toml`), joins them with a bounded wait, flushes state, and
//! explicitly closes both WinDivert handles. The join/close outcome is logged
//! honestly: a timed-out thread or a failed close is reported as partial
//! cleanup (reclaimed on process exit), never as "cleaned up".
//!
//! Capture modes: the engine runs the network handle in sniff mode while no
//! rule is effective and in intercept mode while any rule is effective (see
//! `engine.rs`); the aggregator publishes the desired mode on every rule
//! change, and the engine switches the handle at a packet boundary.

pub mod engine;
pub mod flows;
pub mod rules;
pub mod sockets;
pub mod synthetic;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Context;
use crossbeam_channel::{Receiver, Sender};
use windivert::prelude::*;

use crate::types::{CaptureMode, Command, Rule, Snapshot};

/// State shared across all backend threads.
///
/// Locking discipline: every thread holds at most one of `flows`, `rules`,
/// `buckets` at any instant, so no lock-ordering deadlock is possible.
/// `handle_slot` / `flow_handle` are read (cloned) per engine/drainer
/// iteration and only written by the engine (switch) or the aggregator
/// (shutdown close); the mode atomics are lock-free signals, and
/// `mode_error` is written by the engine only.
pub(crate) struct Shared {
    pub flows: Mutex<flows::FlowTable>,
    pub rules: Mutex<HashMap<String, Rule>>,
    pub buckets: Mutex<HashMap<engine::BucketKey, engine::ShapedBucket>>,
    pub running: AtomicBool,
    /// Desired capture mode, as a `CaptureMode` byte. Written by the
    /// aggregator on startup and on every rule change; read by the engine.
    pub desired_mode: AtomicU8,
    /// Actual capture mode, as a `CaptureMode` byte. Written by the engine
    /// on every successful switch, fallback, or Degraded entry; read by the
    /// aggregator (snapshots) and the engine/drainer (path selection).
    pub actual_mode: AtomicU8,
    /// Sticky mode error (engine-only writer). `None` means no error; the
    /// aggregator copies it into snapshots for the GUI banner.
    pub mode_error: Mutex<Option<String>>,
    /// Current network-layer handle + its generation. The engine replaces
    /// this on a mode switch; the engine and drainer clone it per iteration.
    pub handle_slot: RwLock<Option<(Arc<engine::EngineHandle>, u64)>>,
    /// Monotonic handle generation, incremented on every successful open.
    /// Used for tracing switch history, not as a correctness mechanism (the
    /// switch handshake, not the generation, guarantees exclusivity).
    pub generation: AtomicU64,
    /// Flow-layer handle, shared so the aggregator can wake its blocked
    /// `recv` via `shutdown(Recv)` and close it after joining. `None` when
    /// the flow handle failed to open at startup (flow events then simply
    /// do not run; socket-table refresh still attributes).
    pub flow_handle: RwLock<Option<Arc<FlowHandle>>>,
    /// Capture-thread join handles, moved here at startup so the aggregator
    /// can join them (bounded) on shutdown. Taken exactly once.
    pub capture_handles: Mutex<Option<CaptureHandles>>,
    /// Set by the engine as its very last action, after which it never
    /// receives or enqueues again. The drainer waits for this before its
    /// final flush, so no packet can land in an unflushed queue.
    pub engine_stopped: AtomicBool,
    /// True while the engine is switching handles. The drainer pauses while
    /// this is set (see `drainer_idle`).
    pub switching: AtomicBool,
    /// True while the drainer is paused for a switch (holds no handle clone,
    /// sends nothing). The engine waits for this before closing the old
    /// handle, which is what makes `try_unwrap` succeed.
    pub drainer_idle: AtomicBool,
}

/// The flow-layer WinDivert handle, shared by the flow-events thread (recv)
/// and the aggregator (shutdown wake + post-join close).
///
/// Safety: same reasoning as [`engine::EngineHandle`] — the WinDivert C API
/// documents handles as thread-safe; the Rust wrapper is conservative in not
/// implementing Send/Sync.
pub struct FlowHandle(pub WinDivert<FlowLayer>);
unsafe impl Send for FlowHandle {}
unsafe impl Sync for FlowHandle {}

/// Join handles of the three capture threads. The flow handle is optional:
/// when the flow-layer handle fails to open at startup, no flow thread runs.
pub(crate) struct CaptureHandles {
    pub flow: Option<JoinHandle<()>>,
    pub engine: JoinHandle<()>,
    pub drainer: JoinHandle<()>,
}

/// Which capture threads were actually joined on shutdown. A `false` entry
/// means that thread did not finish within the bounded wait: it was NOT
/// joined (joining would block) and its handle was NOT closed — the shutdown
/// report must say so, not claim a clean exit.
pub(crate) struct JoinOutcome {
    pub flow: bool,
    pub engine: bool,
    pub drainer: bool,
}

/// How long the aggregator waits for all capture threads to finish on
/// shutdown. Threads poll `is_finished` on this interval; a thread that has
/// not finished by the deadline is left running (and reported as such).
const SHUTDOWN_JOIN_WAIT: Duration = Duration::from_secs(5);
/// Poll interval while waiting for capture threads to finish.
const SHUTDOWN_JOIN_POLL: Duration = Duration::from_millis(50);

/// Handle to the running backend. The GUI reads snapshots from `snapshot_rx`.
pub struct Backend {
    /// ~1 Hz stream of state snapshots for the GUI.
    pub snapshot_rx: Receiver<Snapshot>,
    /// The aggregator thread: on shutdown it wakes and joins the capture
    /// threads (bounded), persists rules/history/totals, and closes both
    /// WinDivert handles before returning.
    aggregator: JoinHandle<()>,
}

impl Backend {
    /// Wait for the aggregator to finish. Call this after sending
    /// [`Command::Shutdown`] (or dropping the command sender) and before the
    /// process exits: the aggregator writes `rules.json` on its way out, and
    /// exiting underneath it leaves the file truncated, silently losing every
    /// rule. By the time the aggregator returns, the capture threads have
    /// been joined (bounded) and the handles closed — except for any thread
    /// or handle the shutdown log reports as timed out / unclosed, which is
    /// then reclaimed on process exit.
    pub fn join(self) {
        let _ = self.aggregator.join();
    }
}

/// Start the backend. Consumes the command receiver (GUI to backend) and returns
/// a [`Backend`] exposing the snapshot receiver (backend to GUI).
///
/// Fails early with a clear message if WinDivert cannot be opened (typically:
/// not running elevated, or `WinDivert.dll`/driver missing).
pub fn start(cmd_rx: Receiver<Command>) -> anyhow::Result<Backend> {
    // Load rules first: the initial capture mode is a function of them.
    let rules = rules::load();
    let initial_mode = rules::desired_mode(&rules);
    // Open the single shared network-layer handle up front so failures (not
    // elevated, driver missing) surface as a friendly error. Both the engine
    // and drainer threads use this one handle (see engine::EngineHandle),
    // published in the shared slot for mode switches.
    let engine_handle = Arc::new(
        if initial_mode == CaptureMode::Divert {
            engine::open()
        } else {
            engine::open_sniff()
        }
        .context(
            "无法打开 WinDivert：请以管理员身份运行本程序，并确认 WinDivert.dll \
             与 WinDivert64.sys 和程序在同一目录下",
        )?,
    );
    tracing::info!("initial capture mode: {}", initial_mode.label());

    // Flow-layer handle: unlike the network handle, a failure here must not
    // fail startup (socket-table refresh still attributes). On failure, no
    // flow thread runs and `flow_handle` stays None.
    let flow_handle = match WinDivert::flow("true", 0, WinDivertFlags::new()) {
        Ok(h) => Some(Arc::new(FlowHandle(h))),
        Err(e) => {
            tracing::error!("failed to open WinDivert flow handle (flow events disabled): {e}");
            None
        }
    };

    let shared = Arc::new(Shared {
        flows: Mutex::new(flows::FlowTable::new()),
        rules: Mutex::new(rules),
        buckets: Mutex::new(HashMap::new()),
        running: AtomicBool::new(true),
        desired_mode: AtomicU8::new(initial_mode.to_u8()),
        actual_mode: AtomicU8::new(initial_mode.to_u8()),
        mode_error: Mutex::new(None),
        handle_slot: RwLock::new(Some((engine_handle, 0))),
        generation: AtomicU64::new(0),
        flow_handle: RwLock::new(flow_handle.clone()),
        capture_handles: Mutex::new(None),
        engine_stopped: AtomicBool::new(false),
        switching: AtomicBool::new(false),
        drainer_idle: AtomicBool::new(false),
    });

    // 有界通道（容量 2）：界面只展示最新一帧，GUI 卡顿时旧快照无需
    // 排队堆积；聚合线程用 try_send，满时丢弃当秒快照（见 run_aggregator）。
    let (snapshot_tx, snapshot_rx) = crossbeam_channel::bounded::<Snapshot>(2);

    let flow_thread = match flow_handle {
        Some(handle) => Some(spawn_named("throttle-flow-events", {
            let shared = shared.clone();
            move || run_flow_events(shared, handle)
        })?),
        None => None,
    };

    let engine_thread = spawn_named("throttle-engine", {
        let shared = shared.clone();
        move || engine::run_engine(shared)
    })?;

    let drainer_thread = spawn_named("throttle-drainer", {
        let shared = shared.clone();
        move || engine::run_drainer(shared)
    })?;

    // Hand the capture threads to the aggregator via Shared: it joins them
    // (bounded) on shutdown before persisting and closing the handles.
    *shared.capture_handles.lock().unwrap() = Some(CaptureHandles {
        flow: flow_thread,
        engine: engine_thread,
        drainer: drainer_thread,
    });

    let aggregator = spawn_named("throttle-aggregator", {
        let shared = shared.clone();
        move || run_aggregator(shared, cmd_rx, snapshot_tx)
    })?;

    Ok(Backend {
        snapshot_rx,
        aggregator,
    })
}

fn spawn_named<F>(name: &str, f: F) -> anyhow::Result<JoinHandle<()>>
where
    F: FnOnce() + Send + 'static,
{
    thread::Builder::new()
        .name(name.to_string())
        .spawn(f)
        .with_context(|| format!("failed to spawn thread {name}"))
}

/// How many flow events to log per protocol at startup (diagnostics).
const FLOW_EVENT_LOG_LIMIT: u32 = 10;
/// How often the aggregator re-reads the kernel socket tables so late-created
/// sockets (especially wildcard UDP/QUIC) get attributed.
const TABLE_REFRESH: Duration = Duration::from_secs(2);

/// Flow-event thread: bootstrap existing connections, then track flow
/// establishment/teardown to keep the 5-tuple to process map current.
///
/// The handle is created in [`start`] and shared (so the aggregator can wake
/// its blocked `recv` via `shutdown(Recv)` and close it after joining); this
/// thread never closes it.
fn run_flow_events(shared: Arc<Shared>, handle: Arc<FlowHandle>) {
    // Bootstrap pre-existing TCP connections before we start listening.
    shared.flows.lock().unwrap().bootstrap_tcp();

    let divert = &handle.0;
    tracing::info!("flow-events thread started");

    // Diagnostics: log the first few events of each protocol so a protocol
    // whose 5-tuples never match captured packets is visible in the log.
    let mut logged: HashMap<u8, u32> = HashMap::new();

    while shared.running.load(Ordering::Relaxed) {
        let packet = match divert.recv(None) {
            Ok(p) => p,
            Err(WinDivertError::Recv(WinDivertRecvError::NoData)) => break,
            Err(e) => {
                if !shared.running.load(Ordering::Relaxed) {
                    // Shutdown path: the aggregator woke us via
                    // shutdown(Recv). Leave without touching the handle.
                    break;
                }
                tracing::warn!("flow-events: recv error: {e}");
                continue;
            }
        };

        let addr = &packet.address;
        let protocol = addr.protocol();
        let local = SocketAddr::new(addr.local_address(), addr.local_port());
        let remote = SocketAddr::new(addr.remote_address(), addr.remote_port());
        let pid = addr.process_id();

        let seen = logged.entry(protocol).or_default();
        if *seen < FLOW_EVENT_LOG_LIMIT {
            *seen += 1;
            tracing::info!(
                "flow event: {:?} proto={protocol} local={local} remote={remote} pid={pid}",
                addr.event()
            );
        }

        match addr.event() {
            WinDivertEvent::FlowStablished => {
                shared
                    .flows
                    .lock()
                    .unwrap()
                    .insert_flow(protocol, local, remote, pid);
            }
            WinDivertEvent::FlowDeleted => {
                shared
                    .flows
                    .lock()
                    .unwrap()
                    .remove_flow(protocol, local, remote);
            }
            _ => {}
        }
    }
    tracing::info!("flow-events thread exiting");
}

/// Shutdown step 1: stop the capture threads.
///
/// Sets `running` to false, wakes the network and flow handles' blocked
/// `recv` calls via the patched `WinDivert::shutdown(Recv)` (cross-thread,
/// `&self` — see the `patches/windivert` note in `Cargo.toml`), then joins
/// the capture threads with a bounded wait.
///
/// Join semantics are exact: `is_finished` only reports, `join` reaps. The
/// loop polls `is_finished` until all threads finished or the deadline
/// passes, then calls `join` ONLY on finished threads (joining an unfinished
/// thread would block). A thread that did not finish is left running and
/// reported as such — never described as cleaned up. A `join` error means
/// the thread panicked (logged, still counts as finished).
///
/// On `shutdown(Recv)` semantics: it closes only the receive side; the
/// drainer's exit flush still sends through the same handle afterwards, and
/// the handle itself is closed separately (see [`close_capture_handles`])
/// after the join.
fn shutdown_capture_threads(shared: &Shared) -> JoinOutcome {
    shared.running.store(false, Ordering::Relaxed);

    if let Some((handle, _)) = shared.handle_slot.read().unwrap().clone()
        && let Err(e) = handle.0.shutdown(WinDivertShutdownMode::Recv)
    {
        tracing::warn!("shutdown: network handle shutdown(Recv) failed: {e}");
    }
    if let Some(handle) = shared.flow_handle.read().unwrap().clone()
        && let Err(e) = handle.0.shutdown(WinDivertShutdownMode::Recv)
    {
        tracing::warn!("shutdown: flow handle shutdown(Recv) failed: {e}");
    }

    let Some(handles) = shared.capture_handles.lock().unwrap().take() else {
        tracing::warn!("shutdown: capture handles already taken (no threads to join)");
        return JoinOutcome {
            flow: true,
            engine: false,
            drainer: false,
        };
    };

    // Poll until all finished or the deadline passes.
    let deadline = Instant::now() + SHUTDOWN_JOIN_WAIT;
    loop {
        let flow_done = handles.flow.as_ref().is_none_or(|h| h.is_finished());
        if flow_done && handles.engine.is_finished() && handles.drainer.is_finished() {
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(SHUTDOWN_JOIN_POLL);
    }

    // Join only finished threads (joining an unfinished one would block).
    let CaptureHandles {
        flow,
        engine,
        drainer,
    } = handles;
    let mut outcome = JoinOutcome {
        flow: true,
        engine: false,
        drainer: false,
    };
    if let Some(h) = flow {
        if h.is_finished() {
            if let Err(e) = h.join() {
                tracing::warn!("shutdown: flow thread panicked: {e:?}");
            }
            outcome.flow = true;
        } else {
            tracing::warn!(
                "shutdown: flow thread did NOT finish within {SHUTDOWN_JOIN_WAIT:?}; NOT joined, handle NOT closed (reclaimed on process exit)"
            );
            outcome.flow = false;
        }
    }
    if engine.is_finished() {
        if let Err(e) = engine.join() {
            tracing::warn!("shutdown: engine thread panicked: {e:?}");
        }
        outcome.engine = true;
    } else {
        tracing::warn!(
            "shutdown: engine thread did NOT finish within {SHUTDOWN_JOIN_WAIT:?}; NOT joined, handle NOT closed (reclaimed on process exit)"
        );
    }
    if drainer.is_finished() {
        if let Err(e) = drainer.join() {
            tracing::warn!("shutdown: drainer thread panicked: {e:?}");
        }
        outcome.drainer = true;
    } else {
        tracing::warn!(
            "shutdown: drainer thread did NOT finish within {SHUTDOWN_JOIN_WAIT:?}; NOT joined, handle NOT closed (reclaimed on process exit)"
        );
    }
    outcome
}

/// Shutdown step 2: explicitly close the WinDivert handles, gated on which
/// threads were actually joined.
///
/// The network handle is used by the engine (recv+send) and the drainer
/// (send), so it is closed only when BOTH joined; the flow handle only when
/// the flow thread joined (or never existed). Closing uses unique ownership
/// (`try_unwrap`): if an unexpected holder remains, the close is skipped and
/// logged, not forced. Only a `close` returning `Ok` is reported as
/// "explicitly closed"; anything else is reported as unclosed (reclaimed on
/// process exit), never as cleaned up.
fn close_capture_handles(shared: &Shared, outcome: &JoinOutcome) {
    if outcome.engine && outcome.drainer {
        if let Some((handle, _)) = shared.handle_slot.write().unwrap().take() {
            match Arc::try_unwrap(handle) {
                Ok(mut h) => match h.0.close(CloseAction::Nothing) {
                    Ok(()) => tracing::info!("shutdown: network handle closed explicitly"),
                    Err(e) => tracing::warn!(
                        "shutdown: network handle close failed (reclaimed on process exit): {e}"
                    ),
                },
                Err(_) => tracing::warn!(
                    "shutdown: network handle still shared, skipping close (reclaimed on process exit)"
                ),
            }
        } else {
            tracing::info!("shutdown: no network handle in slot (already closed or Degraded)");
        }
    } else {
        tracing::warn!(
            "shutdown: network handle NOT closed (engine joined={}, drainer joined={}); reclaimed on process exit",
            outcome.engine,
            outcome.drainer
        );
    }

    if outcome.flow {
        if let Some(handle) = shared.flow_handle.write().unwrap().take() {
            match Arc::try_unwrap(handle) {
                Ok(mut h) => match h.0.close(CloseAction::Nothing) {
                    Ok(()) => tracing::info!("shutdown: flow handle closed explicitly"),
                    Err(e) => tracing::warn!(
                        "shutdown: flow handle close failed (reclaimed on process exit): {e}"
                    ),
                },
                Err(_) => tracing::warn!(
                    "shutdown: flow handle still shared, skipping close (reclaimed on process exit)"
                ),
            }
        } else {
            tracing::info!("shutdown: no flow handle (never opened or already closed)");
        }
    } else {
        tracing::warn!(
            "shutdown: flow handle NOT closed (flow thread not joined); reclaimed on process exit"
        );
    }
}

/// Aggregator thread: emit a snapshot every second and apply commands.
///
/// 本 fork 的扩展（都在本线程内完成，开销极小，不阻塞抓包与 UI）：
/// - 每秒把快照喂给告警状态机（`crate::alert`），触发的事件立即写入历史；
/// - 每 60 秒把每进程流量增量写入本地历史文件（`crate::history`），
///   退出时补写最后一个不满 60 秒的周期；
/// - 告警设置（`Command::SetAlertSettings`）在此生效并持久化。
fn run_aggregator(shared: Arc<Shared>, cmd_rx: Receiver<Command>, snapshot_tx: Sender<Snapshot>) {
    tracing::info!("aggregator thread started");
    let ticker = crossbeam_channel::tick(Duration::from_secs(1));
    let refresh = crossbeam_channel::tick(TABLE_REFRESH);

    // --- 本 fork 的附加状态 ---
    let tz_offset = local_utc_offset_secs();
    let hist_path = crate::history::history_path(&rules::config_dir());
    if let Err(e) = crate::history::cleanup_file(&hist_path, now_unix()) {
        tracing::warn!("history cleanup failed: {e}");
    }
    let mut accumulator = crate::history::HistoryAccumulator::new();
    // --- 历史总累计（totals.json，独立于 30 天历史，口径见 crate::totals）---
    let totals_path = crate::totals::totals_path(&rules::config_dir());
    let (mut totals_store, totals_status) = crate::totals::TotalsStore::load(&totals_path);
    match totals_status {
        crate::totals::LoadStatus::Loaded => {
            tracing::info!("已加载历史总累计：{}", totals_path.display());
        }
        crate::totals::LoadStatus::Fresh => {
            tracing::info!("历史总累计文件不存在，从 0 开始累计");
        }
        crate::totals::LoadStatus::Corrupted => {
            tracing::warn!(
                "历史总累计文件损坏，已从 0 重新累计：{}",
                totals_path.display()
            );
        }
    }
    let mut last_totals_tick: u64 = 0;
    let mut alert_settings = rules::load_settings();
    let mut alert_engine = crate::alert::AlertEngine::new(
        alert_settings.up_threshold_bps,
        alert_settings.duration_secs as u64,
        alert_settings.ignore.clone(),
    );
    let mut tick: u64 = 0;
    let mut last_flush_tick: u64 = 0;
    let mut cleanup_day = now_unix() / 86_400;
    // 每个进程键最近一次触发告警的墙钟时间（供横幅显示触发时刻）。
    let mut fired_wall: HashMap<String, i64> = HashMap::new();

    loop {
        crossbeam_channel::select! {
            recv(cmd_rx) -> msg => {
                match msg {
                    Ok(Command::SetRule(mut rule)) => {
                        rule.exe_path = rule.exe_path.to_lowercase();
                        let key = rule.exe_path.clone();
                        let snapshot = {
                            let mut rules = shared.rules.lock().unwrap();
                            rules.insert(key, rule);
                            rules.clone()
                        };
                        if let Err(e) = rules::save(&snapshot) {
                            tracing::warn!("failed to persist rules: {e}");
                        }
                        // Recompute the desired capture mode from the new rule
                        // set: the engine switches the handle at its loop-top.
                        let desired = rules::desired_mode(&snapshot);
                        shared
                            .desired_mode
                            .store(desired.to_u8(), Ordering::Relaxed);
                    }
                    Ok(Command::RemoveRule(path)) => {
                        let key = path.to_lowercase();
                        let snapshot = {
                            let mut rules = shared.rules.lock().unwrap();
                            rules.remove(&key);
                            rules.clone()
                        };
                        // A removed rule may have left shaping buckets behind.
                        // Release them (infinite rate) instead of dropping
                        // them: the drainer flushes the held packets on its
                        // next pass (or the mode-switch flush does, if this
                        // was the last effective rule), so removal never
                        // loses queued packets, and no stale limit lingers
                        // (a re-added rule resets the rate on its next packet).
                        {
                            let mut buckets = shared.buckets.lock().unwrap();
                            for ((exe, _), bucket) in buckets.iter_mut() {
                                if exe == &key {
                                    bucket.release();
                                }
                            }
                        }
                        if let Err(e) = rules::save(&snapshot) {
                            tracing::warn!("failed to persist rules: {e}");
                        }
                        let desired = rules::desired_mode(&snapshot);
                        shared
                            .desired_mode
                            .store(desired.to_u8(), Ordering::Relaxed);
                    }
                    Ok(Command::SetAlertSettings(mut s)) => {
                        s.ignore = s
                            .ignore
                            .iter()
                            .map(|e| e.trim().to_lowercase())
                            .filter(|e| !e.is_empty())
                            .collect();
                        alert_engine.set_config(
                            s.up_threshold_bps,
                            s.duration_secs as u64,
                            s.ignore.clone(),
                        );
                        alert_settings = s;
                        if let Err(e) = rules::save_settings(&alert_settings) {
                            tracing::warn!("failed to persist alert settings: {e}");
                        }
                    }
                    Ok(Command::ResetTotals) => {
                        // 清零历史总累计并立即落盘（只动 totals.json）。
                        totals_store.reset();
                        if let Err(e) = totals_store.save(&totals_path) {
                            tracing::warn!("failed to persist totals after reset: {e}");
                        }
                        tracing::info!("历史总累计已清零");
                    }
                    Ok(Command::Shutdown) | Err(_) => {
                        // Explicit shutdown, or the GUI dropped the command sender.
                        tracing::info!("aggregator: shutting down");
                        // Wake the capture threads' blocked recvs, join them
                        // (bounded), and only then persist: the drainer's
                        // exit flush runs before it returns, so flushed bytes
                        // are included in the totals below.
                        let outcome = shutdown_capture_threads(&shared);
                        // 补写退出前最后一个周期的历史增量（尽力而为）。
                        flush_history(
                            &mut accumulator,
                            &hist_path,
                            tz_offset,
                            tick - last_flush_tick,
                        );
                        // 历史总累计同样补折最后一分钟并落盘（尽力而为）。
                        totals_store.fold();
                        if let Err(e) = totals_store.save(&totals_path) {
                            tracing::warn!("failed to persist totals on shutdown: {e}");
                        }
                        let snapshot = shared.rules.lock().unwrap().clone();
                        if let Err(e) = rules::save(&snapshot) {
                            tracing::warn!("failed to persist rules on shutdown: {e}");
                        }
                        // Explicitly close the handles (gated on which threads
                        // were actually joined); anything unclosed is reported,
                        // not claimed as cleaned.
                        close_capture_handles(&shared, &outcome);
                        break;
                    }
                }
            }
            recv(refresh) -> _ => {
                // Re-read the TCP/UDP owner tables: sockets created after the
                // bootstrap (or never announced via a usable flow event) become
                // attributable here. The four Win32 queries and any PID
                // resolution they trigger run with no lock held - the engine
                // locks `flows` on the packet hot path - and only the finished
                // snapshot is applied under the lock.
                let known = shared.flows.lock().unwrap().known_pids();
                let snap = sockets::query_socket_tables(&known);
                shared.flows.lock().unwrap().apply_socket_snapshot(snap);
            }
            recv(ticker) -> _ => {
                tick += 1;
                let (mut processes, total_down_rate, total_up_rate) =
                    shared.flows.lock().unwrap().snapshot();
                // 展示层友好名解析（按路径缓存，同一路径只读一次元数据，
                // 失败回退 exe 名；不影响任何内部键与匹配逻辑）。
                for p in processes.values_mut() {
                    p.display_name =
                        crate::names::display_name(&p.exe_path, &p.name);
                }
                let now = now_unix();

                // --- 告警判断（纯状态机，开销可忽略） ---
                // 未归属流量（"<unknown>"）不是进程，不参与告警判断；
                // 它的字节仍计入历史与总速率。
                let samples: Vec<crate::alert::ProcSample> = processes
                    .iter()
                    .filter(|(k, _)| k.as_str() != crate::backend::synthetic::UNKNOWN_EXE)
                    .map(|(k, p)| crate::alert::ProcSample {
                        key: k.clone(),
                        pid: p.pid,
                        name: p.name.clone(),
                        exe_path: p.exe_path.clone(),
                        up_rate: p.up_rate,
                    })
                    .collect();
                for ev in alert_engine.update(tick, &samples) {
                    fired_wall.insert(ev.key.clone(), now);
                    tracing::info!(
                        "持续高上传告警: {} (pid {}) 以 {} B/s 上传已持续 {} 秒（阈值 {} B/s）",
                        ev.name, ev.pid, ev.up_rate, ev.sustained_secs, ev.threshold_bps
                    );
                    // 第三条提醒通道：系统弹窗通知（与横幅、历史同一事件，
                    // 每事件只弹一次；发送在独立短线程，失败只记日志）。
                    // 文案用「友好名（exe名）」标签，历史存储仍记 exe 名。
                    let label = crate::names::process_label(&ev.exe_path, &ev.name);
                    crate::notify::notify_alert(
                        &label,
                        ev.pid,
                        ev.up_rate,
                        ev.sustained_secs,
                        ev.threshold_bps,
                    );
                    let rec = crate::history::make_alert_record(
                        now,
                        tz_offset,
                        &ev.name,
                        ev.pid,
                        &ev.exe_path,
                        ev.up_rate,
                        ev.threshold_bps,
                        ev.sustained_secs,
                    );
                    if let Err(e) = crate::history::append_records(&hist_path, &[rec]) {
                        tracing::warn!("failed to append alert history: {e}");
                    }
                }

                // --- 历史累计观测与每 60 秒落盘 ---
                let totals: Vec<(String, crate::history::ProcTotals)> = processes
                    .iter()
                    .map(|(k, p)| {
                        (
                            k.clone(),
                            crate::history::ProcTotals {
                                name: p.name.clone(),
                                pid: p.pid,
                                exe_path: p.exe_path.clone(),
                                down_total: p.down_total,
                                up_total: p.up_total,
                            },
                        )
                    })
                    .collect();
                accumulator.observe(&totals);
                // --- 历史总累计：同样的每秒观测，显示值在此填入快照 ---
                // 显示值 = 已落盘总累计 + 本次运行尚未折入的增量，
                // 口径与重复计数防护见 crate::totals 的模块注释。
                let totals_obs: Vec<(String, u64, u64)> = totals
                    .iter()
                    .map(|(k, t)| (k.clone(), t.down_total, t.up_total))
                    .collect();
                totals_store.observe(&totals_obs);
                for (k, p) in processes.iter_mut() {
                    let (hd, hu) = totals_store.display(k);
                    p.history_down_total = hd;
                    p.history_up_total = hu;
                }
                if tick - last_flush_tick >= 60
                    && flush_history(
                        &mut accumulator,
                        &hist_path,
                        tz_offset,
                        tick - last_flush_tick,
                    )
                {
                    last_flush_tick = tick;
                }
                // 历史总累计与每分钟历史同节拍（60 秒）折入并落盘；
                // 计时独立，历史落盘失败不影响总累计的折入节奏。
                if tick - last_totals_tick >= 60 {
                    totals_store.fold();
                    match totals_store.save(&totals_path) {
                        Ok(()) => last_totals_tick = tick,
                        Err(e) => {
                            tracing::warn!("failed to persist totals: {e}");
                        }
                    }
                }
                // 每天清理一次过期历史。
                let today = now / 86_400;
                if today != cleanup_day {
                    cleanup_day = today;
                    if let Err(e) = crate::history::cleanup_file(&hist_path, now) {
                        tracing::warn!("history cleanup failed: {e}");
                    }
                }

                // --- 横幅用的当前告警（最近触发的一个） ---
                let active_alert = alert_engine
                    .fired()
                    .into_iter()
                    .max_by_key(|f| f.fired_tick)
                    .map(|f| crate::types::ActiveAlert {
                        name: f.name.clone(),
                        display_name: crate::names::process_label(
                            &f.exe_path,
                            &f.name,
                        ),
                        exe_path: f.exe_path.clone(),
                        pid: f.pid,
                        up_rate: processes.get(&f.key).map(|p| p.up_rate).unwrap_or(0),
                        threshold_bps: f.threshold_bps,
                        sustained_secs: tick.saturating_sub(f.above_since),
                        triggered_at: crate::history::format_local_time(
                            fired_wall.get(&f.key).copied().unwrap_or(now),
                            tz_offset,
                        ),
                    });

                // --- 全进程总计（后端聚合，GUI 只展示） ---
                // 本次总计 = 进程表全部行（含合成行）的本次累计之和；
                // 历史总计 = totals 存储的全部已知进程显示值之和。
                let (grand_down_total, grand_up_total) =
                    crate::totals::sum_run_totals(&totals_obs);
                let (history_grand_down_total, history_grand_up_total) =
                    totals_store.grand_display();

                let rules = shared.rules.lock().unwrap().clone();
                let capture_mode =
                    CaptureMode::from_u8(shared.actual_mode.load(Ordering::Relaxed));
                let mode_error = shared.mode_error.lock().unwrap().clone();
                let snapshot = Snapshot {
                    processes,
                    total_down_rate,
                    total_up_rate,
                    grand_down_total,
                    grand_up_total,
                    history_grand_down_total,
                    history_grand_up_total,
                    rules,
                    alert_settings: alert_settings.clone(),
                    active_alert,
                    capture_mode,
                    mode_error,
                };
                match snapshot_tx.try_send(snapshot) {
                    Ok(()) => {}
                    Err(crossbeam_channel::TrySendError::Full(_)) => {
                        // GUI 正忙（拖动窗口等）：丢弃这一秒的快照。
                        // 界面只展示最新一帧，监控与落盘不受影响。
                    }
                    Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                        // GUI receiver gone: nothing left to feed.
                        tracing::info!("aggregator: snapshot receiver dropped, stopping");
                        let outcome = shutdown_capture_threads(&shared);
                        flush_history(
                            &mut accumulator,
                            &hist_path,
                            tz_offset,
                            tick - last_flush_tick,
                        );
                        totals_store.fold();
                        if let Err(e) = totals_store.save(&totals_path) {
                            tracing::warn!("failed to persist totals on stop: {e}");
                        }
                        // Save rules too (same shutdown path as
                        // Command::Shutdown; previously this arm skipped it).
                        let snapshot = shared.rules.lock().unwrap().clone();
                        if let Err(e) = rules::save(&snapshot) {
                            tracing::warn!("failed to persist rules on stop: {e}");
                        }
                        close_capture_handles(&shared, &outcome);
                        break;
                    }
                }
            }
        }
    }
    tracing::info!("aggregator thread exiting");
}

/// 把累计器中尚未落盘的增量写入历史文件。成功返回 true 并推进基线；
/// 失败只记日志，下个周期会连同新数据一并重试（平均速率按实际周期算）。
fn flush_history(
    accumulator: &mut crate::history::HistoryAccumulator,
    hist_path: &std::path::Path,
    tz_offset: i64,
    period_secs: u64,
) -> bool {
    if period_secs == 0 {
        return true;
    }
    let rows = accumulator.build_rows(now_unix(), tz_offset, period_secs);
    if rows.is_empty() {
        accumulator.mark_written();
        return true;
    }
    match crate::history::append_records(hist_path, &rows) {
        Ok(()) => {
            accumulator.mark_written();
            true
        }
        Err(e) => {
            tracing::warn!("failed to append history: {e}");
            false
        }
    }
}

/// 当前 unix 时间戳（秒）。
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 本机 UTC 偏移（秒）：本地时间 = UTC + 偏移。
#[cfg(windows)]
fn local_utc_offset_secs() -> i64 {
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    unsafe {
        let mut tzi = TIME_ZONE_INFORMATION::default();
        let id = GetTimeZoneInformation(&mut tzi);
        // UTC = 本地时间 + Bias + 当前生效的附加偏差（分钟），
        // 故本地偏移（秒）= -(Bias + 附加偏差) * 60。
        // 返回值 2 = TIME_ZONE_ID_DAYLIGHT（夏令时生效中）。
        let extra = if id == 2 {
            tzi.DaylightBias
        } else {
            tzi.StandardBias
        };
        -((tzi.Bias + extra) as i64) * 60
    }
}

#[cfg(not(windows))]
fn local_utc_offset_secs() -> i64 {
    0
}
