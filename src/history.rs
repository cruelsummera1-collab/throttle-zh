//! 本地历史记录：JSON Lines 落盘（每行一条 JSON 记录），仅依赖 std。
//!
//! 记录分三类（`kind` 字段）：
//! - `minute`：每 60 秒一条的每进程周期增量（该周期内的上下行字节数
//!   与平均速率），用于事后按时间追溯哪个进程在传数据；
//! - `total`：同一时刻的全机汇总增量（所有已归属进程增量之和）；
//! - `alert`：告警事件（精确时间、进程、阈值、触发速率、持续秒数）。
//!
//! 累计口径：落盘的是"周期增量"（当前累计 − 已落盘累计），与界面上的
//! "本次运行累计"是两个概念，互不相加；进程计数器回退（同路径进程
//! 重启导致累计归零）时以当前累计作为本期增量，避免重复累计或丢数。
//!
//! 容错：文件不存在视为空；损坏行在读取/清理时被跳过；写入失败只由
//! 调用方记日志，绝不影响实时监控。保留策略：最近 30 天，且文件
//! 不超过 20 MiB（超出时从最旧的开始丢弃）。

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

/// 历史文件名（位于应用数据目录内）。
pub const HISTORY_FILE_NAME: &str = "history.jsonl";
/// 保留天数。
pub const RETENTION_DAYS: i64 = 30;
/// 文件大小上限（字节）。
pub const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
/// 全机汇总行的进程名标记。
pub const TOTAL_NAME: &str = "全部进程";

/// 一条历史记录。数值字段的含义随 `kind` 而定（见模块文档）。
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryRecord {
    /// unix 时间戳（秒）。
    pub ts: i64,
    /// 本地时间字符串 "YYYY-MM-DD HH:MM:SS"。
    pub time: String,
    /// minute | total | alert
    pub kind: String,
    pub name: String,
    pub pid: u32,
    pub exe_path: String,
    /// minute/total：本周期增量字节；alert：0。
    pub down_bytes: u64,
    pub up_bytes: u64,
    /// minute/total：本周期平均速率（字节/秒）；alert：0。
    pub down_rate: u64,
    /// minute/total：本周期平均速率；alert：触发瞬间上传速率。
    pub up_rate: u64,
    /// alert 专用：触发阈值（字节/秒）；其余为 0。
    pub threshold_bps: u64,
    /// alert 专用：已持续秒数；其余为 0。
    pub sustained_secs: u64,
}

/// 构造一条告警事件记录。
pub fn make_alert_record(
    ts: i64,
    utc_offset_secs: i64,
    name: &str,
    pid: u32,
    exe_path: &str,
    up_rate: u64,
    threshold_bps: u64,
    sustained_secs: u64,
) -> HistoryRecord {
    HistoryRecord {
        ts,
        time: format_local_time(ts, utc_offset_secs),
        kind: "alert".to_string(),
        name: name.to_string(),
        pid,
        exe_path: exe_path.to_string(),
        down_bytes: 0,
        up_bytes: 0,
        down_rate: 0,
        up_rate,
        threshold_bps,
        sustained_secs,
    }
}

// ---------------------------------------------------------------------------
// 时间格式化（unix 秒 + UTC 偏移 → 本地时间字符串），纯历法推算。
// ---------------------------------------------------------------------------

/// 把 unix 时间戳按给定 UTC 偏移（秒）格式化为 "YYYY-MM-DD HH:MM:SS"。
pub fn format_local_time(ts: i64, utc_offset_secs: i64) -> String {
    let local = ts + utc_offset_secs;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Howard Hinnant 的 days→civil 算法（公历，含闰年）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------------------
// JSON 序列化 / 宽容解析（只处理本模块写出的字段布局）
// ---------------------------------------------------------------------------

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

impl HistoryRecord {
    /// 序列化为一行 JSON（无尾随换行）。
    pub fn to_json_line(&self) -> String {
        format!(
            "{{\"ts\":{},\"time\":\"{}\",\"kind\":\"{}\",\"name\":\"{}\",\"pid\":{},\"exe_path\":\"{}\",\"down_bytes\":{},\"up_bytes\":{},\"down_rate\":{},\"up_rate\":{},\"threshold_bps\":{},\"sustained_secs\":{}}}",
            self.ts,
            json_escape(&self.time),
            json_escape(&self.kind),
            json_escape(&self.name),
            self.pid,
            json_escape(&self.exe_path),
            self.down_bytes,
            self.up_bytes,
            self.down_rate,
            self.up_rate,
            self.threshold_bps,
            self.sustained_secs,
        )
    }

    /// 宽容解析一行 JSON：关键字段缺失或格式损坏时返回 None。
    pub fn from_json_line(line: &str) -> Option<HistoryRecord> {
        let ts = extract_i64(line, "\"ts\"")?;
        let kind = extract_string(line, "\"kind\"")?;
        Some(HistoryRecord {
            ts,
            time: extract_string(line, "\"time\"").unwrap_or_default(),
            kind,
            name: extract_string(line, "\"name\"").unwrap_or_default(),
            pid: extract_u64(line, "\"pid\"").unwrap_or(0) as u32,
            exe_path: extract_string(line, "\"exe_path\"").unwrap_or_default(),
            down_bytes: extract_u64(line, "\"down_bytes\"").unwrap_or(0),
            up_bytes: extract_u64(line, "\"up_bytes\"").unwrap_or(0),
            down_rate: extract_u64(line, "\"down_rate\"").unwrap_or(0),
            up_rate: extract_u64(line, "\"up_rate\"").unwrap_or(0),
            threshold_bps: extract_u64(line, "\"threshold_bps\"").unwrap_or(0),
            sustained_secs: extract_u64(line, "\"sustained_secs\"").unwrap_or(0),
        })
    }
}

/// 找到 `"key":` 后紧跟的数值（允许空白）。找不到返回 None。
fn find_value_start(line: &str, key: &str) -> Option<usize> {
    let pos = line.find(key)?;
    let rest = &line[pos + key.len()..];
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let skipped = after.len() - after.trim_start().len();
    Some(pos + key.len() + colon + 1 + skipped)
}

fn extract_i64(line: &str, key: &str) -> Option<i64> {
    let start = find_value_start(line, key)?;
    let tail = &line[start..];
    let end = tail
        .find(|c: char| !(c.is_ascii_digit() || c == '-'))
        .unwrap_or(tail.len());
    tail[..end].parse::<i64>().ok()
}

fn extract_u64(line: &str, key: &str) -> Option<u64> {
    extract_i64(line, key).and_then(|v| u64::try_from(v).ok())
}

/// 提取 `"key":"..."` 的字符串值，处理常见转义。非字符串/损坏返回 None。
fn extract_string(line: &str, key: &str) -> Option<String> {
    let start = find_value_start(line, key)?;
    let tail = line[start..].strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = tail.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => {
                let _ = i;
                return Some(out);
            }
            '\\' => match chars.next() {
                Some((_, 'n')) => out.push('\n'),
                Some((_, 'r')) => out.push('\r'),
                Some((_, 't')) => out.push('\t'),
                Some((_, '"')) => out.push('"'),
                Some((_, '\\')) => out.push('\\'),
                Some((_, '/')) => out.push('/'),
                Some((_, 'u')) => {
                    let hex: String = chars.by_ref().take(4).map(|(_, c)| c).collect();
                    if let Some(n) = u32::from_str_radix(&hex, 16)
                        .ok()
                        .and_then(char::from_u32)
                    {
                        out.push(n);
                    }
                }
                _ => return None,
            },
            c => out.push(c),
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 周期增量累计器
// ---------------------------------------------------------------------------

/// 某进程在某一时刻的累计计数（由快照转换而来）。
#[derive(Debug, Clone, PartialEq)]
pub struct ProcTotals {
    pub name: String,
    pub pid: u32,
    pub exe_path: String,
    /// 自（后端）启动跟踪以来的累计字节（与界面"本次运行累计"同口径）。
    pub down_total: u64,
    pub up_total: u64,
}

/// 把每秒快照的累计值转换为"每周期增量"记录。
///
/// 用法：每秒 `observe` 一次；每 60 秒 `build_rows` 生成记录，写入成功
/// 后调用 `mark_written` 推进基线；写入失败则不调用，下个周期会把
/// 未落盘的增量一并带上（周期秒数随之变长，平均速率仍正确）。
pub struct HistoryAccumulator {
    /// 已确认落盘的累计基线。
    written: HashMap<String, (u64, u64)>,
    /// 最近一次观测到的累计值（含已退出进程的最后数值）。
    last_seen: HashMap<String, ProcTotals>,
    /// 最近一次观测时仍在运行的进程键集合。
    present: HashSet<String>,
}

fn counter_delta(written: u64, current: u64) -> u64 {
    if current >= written {
        current - written
    } else {
        // 计数器回退（同路径进程重启等）：本周期至少传了 current 这么多。
        current
    }
}

impl HistoryAccumulator {
    pub fn new() -> Self {
        Self {
            written: HashMap::new(),
            last_seen: HashMap::new(),
            present: HashSet::new(),
        }
    }

    /// 记录一次观测（键为快照键，通常是小写 exe 路径）。
    pub fn observe(&mut self, procs: &[(String, ProcTotals)]) {
        self.present = procs.iter().map(|(k, _)| k.clone()).collect();
        for (k, t) in procs {
            self.last_seen.insert(k.clone(), t.clone());
        }
    }

    /// 生成本周期记录：每个有增量的进程一条 minute 记录 + 一条 total。
    /// `period_secs` 是距上次成功落盘的秒数（用于平均速率）。
    pub fn build_rows(
        &self,
        ts: i64,
        utc_offset_secs: i64,
        period_secs: u64,
    ) -> Vec<HistoryRecord> {
        let period = period_secs.max(1);
        let time = format_local_time(ts, utc_offset_secs);
        let mut rows = Vec::new();
        let mut total_down: u64 = 0;
        let mut total_up: u64 = 0;
        // 排序保证输出稳定（也便于测试与人工查看）。
        let mut keys: Vec<&String> = self.last_seen.keys().collect();
        keys.sort();
        for key in keys {
            let t = &self.last_seen[key];
            let (wd, wu) = self.written.get(key).copied().unwrap_or((0, 0));
            let dd = counter_delta(wd, t.down_total);
            let du = counter_delta(wu, t.up_total);
            if dd == 0 && du == 0 {
                continue;
            }
            total_down += dd;
            total_up += du;
            rows.push(HistoryRecord {
                ts,
                time: time.clone(),
                kind: "minute".to_string(),
                name: t.name.clone(),
                pid: t.pid,
                exe_path: t.exe_path.clone(),
                down_bytes: dd,
                up_bytes: du,
                down_rate: dd / period,
                up_rate: du / period,
                threshold_bps: 0,
                sustained_secs: 0,
            });
        }
        if total_down > 0 || total_up > 0 {
            rows.push(HistoryRecord {
                ts,
                time: time.clone(),
                kind: "total".to_string(),
                name: TOTAL_NAME.to_string(),
                pid: 0,
                exe_path: String::new(),
                down_bytes: total_down,
                up_bytes: total_up,
                down_rate: total_down / period,
                up_rate: total_up / period,
                threshold_bps: 0,
                sustained_secs: 0,
            });
        }
        rows
    }

    /// 落盘成功后推进基线，并清掉已退出进程的残留状态。
    pub fn mark_written(&mut self) {
        for (k, t) in &self.last_seen {
            self.written.insert(k.clone(), (t.down_total, t.up_total));
        }
        self.last_seen.retain(|k, _| self.present.contains(k));
        self.written.retain(|k, _| self.present.contains(k));
    }
}

impl Default for HistoryAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 文件操作
// ---------------------------------------------------------------------------

/// 数据目录下的历史文件完整路径。
pub fn history_path(dir: &Path) -> PathBuf {
    dir.join(HISTORY_FILE_NAME)
}

/// 追加一批记录（自动创建目录）。失败返回 Err，由调用方记日志。
pub fn append_records(path: &Path, records: &[HistoryRecord]) -> io::Result<()> {
    if records.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let mut buf = String::new();
    for r in records {
        buf.push_str(&r.to_json_line());
        buf.push('\n');
    }
    file.write_all(buf.as_bytes())?;
    Ok(())
}

/// 清理：删除 30 天前的记录，并把文件压到大小上限以内（从最旧丢起）。
/// 文件不存在视为成功；损坏行直接丢弃。本函数整体重写文件，调用方
/// 应在启动时或每天至多调用一次。
pub fn cleanup_file(path: &Path, now_ts: i64) -> io::Result<()> {
    let data = match std::fs::read_to_string(path) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let cutoff = now_ts - RETENTION_DAYS * 86_400;
    let mut kept: Vec<String> = Vec::new();
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match HistoryRecord::from_json_line(line) {
            Some(r) if r.ts >= cutoff => kept.push(line.to_string()),
            Some(_) => {} // 过期，丢弃
            None => {}    // 损坏行，丢弃
        }
    }
    // 大小上限：从最旧的开始丢。
    let mut total_bytes: usize = kept.iter().map(|l| l.len() + 1).sum();
    let mut start = 0;
    while total_bytes as u64 > MAX_FILE_BYTES && start < kept.len() {
        total_bytes -= kept[start].len() + 1;
        start += 1;
    }
    let kept = &kept[start..];
    let mut out = String::new();
    for l in kept {
        out.push_str(l);
        out.push('\n');
    }
    std::fs::write(path, out)?;
    Ok(())
}

/// 读回最近 `max` 条记录（时间正序）。文件不存在/损坏行自动跳过。
/// 只读取文件尾部约 512 KiB，避免大文件整表读入。
pub fn read_recent(path: &Path, max: usize) -> Vec<HistoryRecord> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    const TAIL: u64 = 512 * 1024;
    let mut buf = Vec::new();
    if len > TAIL {
        if file.seek(SeekFrom::Start(len - TAIL)).is_err() {
            return Vec::new();
        }
        if file.read_to_end(&mut buf).is_err() {
            return Vec::new();
        }
        // 丢弃被截断的第一行。
        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            buf.drain(..=pos);
        }
    } else if file.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut records: Vec<HistoryRecord> = text
        .lines()
        .filter_map(|l| HistoryRecord::from_json_line(l.trim()))
        .collect();
    if records.len() > max {
        records.drain(..records.len() - max);
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_timestamps() {
        assert_eq!(format_local_time(0, 0), "1970-01-01 00:00:00");
        assert_eq!(format_local_time(0, 8 * 3600), "1970-01-01 08:00:00");
        // 1700000000 = 2023-11-14 22:13:20 UTC
        assert_eq!(format_local_time(1_700_000_000, 0), "2023-11-14 22:13:20");
        assert_eq!(
            format_local_time(1_700_000_000, 8 * 3600),
            "2023-11-15 06:13:20"
        );
        // 闰日：1709164800 = 2024-02-29 00:00:00 UTC
        assert_eq!(format_local_time(1_709_164_800, 0), "2024-02-29 00:00:00");
    }

    #[test]
    fn record_json_round_trip_with_tricky_strings() {
        let r = HistoryRecord {
            ts: 1_700_000_000,
            time: "2023-11-14 22:13:20".to_string(),
            kind: "minute".to_string(),
            name: "weird \"quoted\" \\ 进程.exe".to_string(),
            pid: 4242,
            exe_path: "c:\\dir\\进程.exe".to_string(),
            down_bytes: 100,
            up_bytes: 200,
            down_rate: 3,
            up_rate: 6,
            threshold_bps: 0,
            sustained_secs: 0,
        };
        let line = r.to_json_line();
        assert!(!line.contains('\n'));
        let back = HistoryRecord::from_json_line(&line).expect("parse own output");
        assert_eq!(r, back);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(HistoryRecord::from_json_line("").is_none());
        assert!(HistoryRecord::from_json_line("not json").is_none());
        assert!(HistoryRecord::from_json_line("{\"kind\":\"minute\"}").is_none()); // 无 ts
        assert!(HistoryRecord::from_json_line("{\"ts\":123}").is_none()); // 无 kind
    }

    fn totals(name: &str, pid: u32, down: u64, up: u64) -> (String, ProcTotals) {
        (
            format!("c:\\apps\\{name}"),
            ProcTotals {
                name: name.to_string(),
                pid,
                exe_path: format!("c:\\apps\\{name}"),
                down_total: down,
                up_total: up,
            },
        )
    }

    #[test]
    fn accumulator_writes_deltas_not_cumulative() {
        let mut acc = HistoryAccumulator::new();
        acc.observe(&[totals("a.exe", 1, 6000, 3000), totals("b.exe", 2, 0, 0)]);
        let rows = acc.build_rows(1_700_000_000, 0, 60);
        // b.exe 无增量被跳过；a.exe 一条 + total 一条。
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, "minute");
        assert_eq!(rows[0].down_bytes, 6000);
        assert_eq!(rows[0].up_rate, 50);
        assert_eq!(rows[1].kind, "total");
        assert_eq!(rows[1].down_bytes, 6000);
        acc.mark_written();

        // 第二周期：只写新增量，不重复累计。
        acc.observe(&[totals("a.exe", 1, 9000, 3000), totals("b.exe", 2, 120, 0)]);
        let rows = acc.build_rows(1_700_000_060, 0, 60);
        assert_eq!(rows.len(), 3); // a、b、total
        let a = rows.iter().find(|r| r.name == "a.exe").unwrap();
        assert_eq!(a.down_bytes, 3000);
        assert_eq!(a.up_bytes, 0);
        let total = rows.iter().find(|r| r.kind == "total").unwrap();
        assert_eq!(total.down_bytes, 3120);
    }

    #[test]
    fn accumulator_flushes_final_delta_of_exited_process() {
        let mut acc = HistoryAccumulator::new();
        acc.observe(&[totals("a.exe", 1, 1000, 0)]);
        acc.build_rows(1_700_000_000, 0, 60);
        acc.mark_written();
        // 进程又传了 500 字节后退出：下一周期仍应记下最后增量，然后清干净。
        acc.observe(&[totals("a.exe", 1, 1500, 0)]);
        acc.observe(&[]);
        let rows = acc.build_rows(1_700_000_060, 0, 60);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].down_bytes, 500);
        acc.mark_written();
        // 再下一周期：无任何记录。
        acc.observe(&[]);
        assert!(acc.build_rows(1_700_000_120, 0, 60).is_empty());
    }

    #[test]
    fn accumulator_handles_counter_reset() {
        let mut acc = HistoryAccumulator::new();
        acc.observe(&[totals("a.exe", 1, 10_000, 0)]);
        acc.build_rows(1_700_000_000, 0, 60);
        acc.mark_written();
        // 同路径新进程，累计从 0 重新开始：以当前值作为增量。
        acc.observe(&[totals("a.exe", 9, 700, 0)]);
        let rows = acc.build_rows(1_700_000_060, 0, 60);
        assert_eq!(rows[0].down_bytes, 700);
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "throttle-history-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn store_append_read_cleanup_cycle() {
        let dir = temp_dir("cycle");
        let path = history_path(&dir);
        // 首次运行：文件不存在时读回为空、清理成功。
        assert!(read_recent(&path, 10).is_empty());
        cleanup_file(&path, 1_700_000_000).expect("cleanup on missing file");

        let old = make_alert_record(1_600_000_000, 0, "old.exe", 1, "c:\\old.exe", 10, 100, 30);
        let new = make_alert_record(1_700_000_000, 0, "new.exe", 2, "c:\\new.exe", 20, 100, 45);
        append_records(&path, &[old, new.clone()]).expect("append");
        // 追加一条损坏行，不应影响读回。
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "{{corrupt").unwrap();
        }
        let read = read_recent(&path, 10);
        assert_eq!(read.len(), 2);
        assert_eq!(read[1], new);

        // 清理：2023-11-14 为 "now" 时，2020 年的旧记录被删除，新记录保留。
        cleanup_file(&path, 1_700_000_000).expect("cleanup");
        let read = read_recent(&path, 10);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].name, "new.exe");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_failure_is_error_not_panic() {
        // 把目录位置占成普通文件，制造不可写环境。
        let dir = temp_dir("blocked");
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let path = blocker.join("history.jsonl");
        let rec = make_alert_record(1_700_000_000, 0, "a.exe", 1, "c:\\a.exe", 1, 1, 1);
        assert!(append_records(&path, &[rec]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
