//! Shared types between the capture/limiter backend and the GUI.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Seconds of rate history kept per process for sparklines.
pub const HISTORY_LEN: usize = 60;

/// Capture mode of the network-layer WinDivert handle.
///
/// The mode is a pure function of the rule set (see
/// `crate::backend::rules::desired_mode`): Divert while any rule is effective,
/// Sniff otherwise. `Degraded` is not a desired mode — it only appears when
/// no handle could be opened at all, and it is always accompanied by a sticky
/// `mode_error` explaining why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CaptureMode {
    /// Sniff-only: the driver hands us copies of packets; the originals keep
    /// flowing through the stack and are never re-injected. Used when no
    /// effective throttle/block rule exists, so monitoring costs no
    /// intercept/re-inject round-trip on the traffic path.
    #[default]
    Sniff,
    /// Intercept: packets are diverted, shaped (or dropped) and re-injected
    /// by the engine/drainer. Required while any effective rule exists.
    Divert,
    /// No working handle: neither capture nor shaping is active.
    Degraded,
}

impl CaptureMode {
    /// Compact encoding for lock-free sharing via `AtomicU8`.
    pub fn to_u8(self) -> u8 {
        match self {
            CaptureMode::Sniff => 0,
            CaptureMode::Divert => 1,
            CaptureMode::Degraded => 2,
        }
    }

    /// Inverse of [`CaptureMode::to_u8`]; unknown values map to Sniff (the
    /// least invasive mode) rather than panicking.
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => CaptureMode::Divert,
            2 => CaptureMode::Degraded,
            _ => CaptureMode::Sniff,
        }
    }

    /// 界面与日志用的中文模式名。
    pub fn label(self) -> &'static str {
        match self {
            CaptureMode::Sniff => "监控",
            CaptureMode::Divert => "拦截",
            CaptureMode::Degraded => "异常",
        }
    }
}

/// A per-process traffic rule, matched by lowercase executable path.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    /// Full lowercase path of the executable this rule applies to.
    pub exe_path: String,
    /// Download limit in bytes/sec. `None` = unlimited.
    pub down_limit: Option<u64>,
    /// Upload limit in bytes/sec. `None` = unlimited.
    pub up_limit: Option<u64>,
    /// If true, all traffic for this process is dropped.
    pub blocked: bool,
}

/// Live stats for one process, sent to the GUI once per second.
#[derive(Debug, Clone, Default)]
pub struct ProcessStats {
    pub pid: u32,
    /// Full executable path (may be empty if resolution failed).
    pub exe_path: String,
    /// File name portion (internal identity; rules/alerts/history key on it).
    pub name: String,
    /// 中文友好显示名（由聚合线程经 `crate::names` 解析后填入，可能与
    /// `name` 相同；仅用于展示，不参与任何匹配逻辑）。
    pub display_name: String,
    /// Current download rate, bytes/sec.
    pub down_rate: u64,
    /// Current upload rate, bytes/sec.
    pub up_rate: u64,
    /// Total bytes since start.
    pub down_total: u64,
    pub up_total: u64,
    /// 历史总累计（跨运行，由 `crate::totals` 按分钟增量折入落盘后，
    /// 聚合线程在每秒快照时填入“已落盘 + 本次未折入增量”的显示值）。
    pub history_down_total: u64,
    pub history_up_total: u64,
    /// Rolling per-second download rates, newest last, up to HISTORY_LEN.
    pub down_history: Vec<u64>,
    /// Rolling per-second upload rates, newest last, up to HISTORY_LEN.
    pub up_history: Vec<u64>,
    /// Number of active flows attributed to this process.
    pub flow_count: usize,
}

/// 异常上传提醒设置（持久化到数据目录的 `settings.json`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertSettings {
    /// 单进程上传速率阈值（字节/秒）；0 表示关闭提醒。
    pub up_threshold_bps: u64,
    /// 连续达到阈值的秒数才触发提醒。
    pub duration_secs: u32,
    /// 忽略名单：进程名或完整路径（不区分大小写）；默认为空，
    /// 不预置任何程序（包括爱奇艺等可能需要检查的程序）。
    pub ignore: Vec<String>,
}

impl Default for AlertSettings {
    fn default() -> Self {
        Self {
            // 500 KB/s（500 × 1024 字节/秒），与界面速率显示同为 1024 进制。
            up_threshold_bps: 500 * 1024,
            duration_secs: 30,
            ignore: Vec::new(),
        }
    }
}

/// 当前正在持续（已触发且尚未恢复）的上传告警，供界面横幅展示。
#[derive(Debug, Clone, Default)]
pub struct ActiveAlert {
    pub name: String,
    /// 「友好名（exe名）」形式的展示标签（与横幅/弹窗文案一致）。
    pub display_name: String,
    pub exe_path: String,
    pub pid: u32,
    /// 当前上传速率（字节/秒）。
    pub up_rate: u64,
    /// 触发阈值（字节/秒）。
    pub threshold_bps: u64,
    /// 已持续秒数（自首次达标起算）。
    pub sustained_secs: u64,
    /// 触发时刻的本地时间字符串。
    pub triggered_at: String,
}

/// Snapshot of everything the GUI needs, produced ~1x/sec by the backend.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Keyed by lowercase exe_path (processes aggregated across PIDs share a key).
    pub processes: HashMap<String, ProcessStats>,
    /// Total current rates across all processes, bytes/sec.
    pub total_down_rate: u64,
    pub total_up_rate: u64,
    /// 本次运行全进程累计之和（后端聚合，含“系统”“未归属流量”行，
    /// 与进程表全部行的本次累计口径一致；GUI 只展示不自行求和）。
    pub grand_down_total: u64,
    pub grand_up_total: u64,
    /// 历史总累计的全进程之和（totals 落盘值 + 本次未折入增量，
    /// 含本次运行未出现的进程；后端聚合，GUI 只展示）。
    pub history_grand_down_total: u64,
    pub history_grand_up_total: u64,
    /// Currently active rules, keyed by lowercase exe_path.
    pub rules: HashMap<String, Rule>,
    /// 当前生效的异常上传提醒设置。
    pub alert_settings: AlertSettings,
    /// 正在持续的上传告警（最近触发的一个）；无告警时为 None。
    pub active_alert: Option<ActiveAlert>,
    /// 网络层捕获的实际模式（engine 维护；切换瞬态内仍报旧值）。
    pub capture_mode: CaptureMode,
    /// 粘性模式错误（engine 写入）：切换失败或句柄均打开失败时非空，
    /// 界面据此显示横幅；只有成功切换或期望模式重新等于实际模式时清除。
    /// 无错误时为 None（切换瞬态的期望/实际失配不算错误，不显示横幅）。
    pub mode_error: Option<String>,
}

/// Commands sent from the GUI to the backend.
#[derive(Debug, Clone)]
pub enum Command {
    /// Insert or replace the rule for `rule.exe_path`.
    SetRule(Rule),
    /// Remove any rule for this lowercase exe path.
    RemoveRule(String),
    /// Replace the alert settings and persist them.
    SetAlertSettings(AlertSettings),
    /// 清零历史总累计（totals.json），立即生效并落盘；不动每分钟
    /// 历史记录与告警事件。
    ResetTotals,
    /// Stop the engine threads and flush state (app exit).
    Shutdown,
}
