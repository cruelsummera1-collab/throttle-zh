//! 历史总累计：每个进程（键 = 小写 exe 完整路径，与主表行身份一致）
//! 自累计以来的总下载/总上传，独立落盘为数据目录下的 `totals.json`。
//!
//! 口径（与 `history.rs` 的周期增量完全一致，杜绝重复计数）：
//! - 维护三份状态：已落盘总累计 `persisted`、已被折入落盘的运行累计
//!   基线 `baseline`、最近一次观测到的运行累计 `last_seen`。
//! - 显示值 = `persisted + counter_delta(baseline, last_seen)`：已落盘
//!   的部分（往次运行 + 本次运行已折入的分钟）在 `persisted` 里，本次
//!   运行尚未折入的增量由后半段补上，两部分不重叠、不会重复计入。
//! - 每分钟（聚合线程节拍）`fold` 一次：把待折增量折入 `persisted`
//!   并推进基线，随后落盘。崩溃/断电最多丢最后一分钟的未折增量。
//! - 与 history.jsonl 的 30 天保留完全独立：永不自动清零；只能由
//!   用户在历史窗口手动清零（`reset`）。
//!
//! 容错：文件不存在 = 从 0 开始（`LoadStatus::Fresh`）；文件损坏或
//! 读取失败 = 从 0 重新累计（`LoadStatus::Corrupted`，由调用方记一条
//! 日志）。落盘先写 `.tmp` 再 rename 覆盖，避免写一半损坏原文件。
//! 本模块仅依赖 std，可独立单元测试。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

/// 总累计文件名（位于应用数据目录内，与 history.jsonl 同目录）。
pub const TOTALS_FILE_NAME: &str = "totals.json";
/// 文件格式版本（写入 version 字段；解析时不据此拒绝未知版本，
/// 只取 totals 内容，保证向前兼容）。
pub const FORMAT_VERSION: u32 = 1;

/// 启动时加载 totals 文件的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadStatus {
    /// 文件不存在：第一次运行，从 0 开始。
    Fresh,
    /// 文件正常读回。
    Loaded,
    /// 文件存在但损坏/不可读：从 0 重新累计，调用方应记一条日志。
    Corrupted,
}

/// 计数器增量：与 history.rs 同口径——计数器回退（同路径进程重启
/// 导致运行累计归零）时，以当前值作为增量，避免丢数或负数。
fn counter_delta(baseline: u64, current: u64) -> u64 {
    if current >= baseline {
        current - baseline
    } else {
        current
    }
}

/// 历史总累计存储（聚合线程持有，每秒 observe、每分钟 fold+save）。
pub struct TotalsStore {
    /// 已落盘的总累计：键 → (总下载, 总上传)。
    persisted: BTreeMap<String, (u64, u64)>,
    /// 已折入 persisted 的运行累计基线：键 → (下载累计, 上传累计)。
    baseline: HashMap<String, (u64, u64)>,
    /// 最近一次观测到的运行累计（本次运行口径，与界面一致）。
    last_seen: HashMap<String, (u64, u64)>,
    /// 最近一次观测时快照里出现的键集合。
    present: HashSet<String>,
}

impl TotalsStore {
    pub fn new() -> Self {
        Self {
            persisted: BTreeMap::new(),
            baseline: HashMap::new(),
            last_seen: HashMap::new(),
            present: HashSet::new(),
        }
    }

    /// 从 totals.json 加载。任何失败都不返回 Err：不存在 → Fresh，
    /// 损坏/不可读 → Corrupted 且从 0 开始。
    pub fn load(path: &Path) -> (Self, LoadStatus) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return (Self::new(), LoadStatus::Fresh);
            }
            Err(_) => return (Self::new(), LoadStatus::Corrupted),
        };
        match parse_persisted(&text) {
            Some(map) => {
                let mut store = Self::new();
                store.persisted = map;
                (store, LoadStatus::Loaded)
            }
            None => (Self::new(), LoadStatus::Corrupted),
        }
    }

    /// 记录一次观测：`current` 是 (键, 本次运行累计下载, 本次运行累计上传)。
    pub fn observe(&mut self, current: &[(String, u64, u64)]) {
        self.present = current.iter().map(|(k, _, _)| k.clone()).collect();
        for (k, down, up) in current {
            self.last_seen.insert(k.clone(), (*down, *up));
        }
    }

    /// 某键当前应显示的历史总累计（已落盘 + 本次运行未折入增量）。
    pub fn display(&self, key: &str) -> (u64, u64) {
        let p = self.persisted.get(key).copied().unwrap_or((0, 0));
        let Some(cur) = self.last_seen.get(key).copied() else {
            return p;
        };
        let base = self.baseline.get(key).copied().unwrap_or((0, 0));
        (
            p.0.saturating_add(counter_delta(base.0, cur.0)),
            p.1.saturating_add(counter_delta(base.1, cur.1)),
        )
    }

    /// 把待折增量折入 persisted 并推进基线；随后清掉已不在快照中的
    /// 键的基线/观测残留（其最后增量已在本次折入，不会丢）。
    pub fn fold(&mut self) {
        for (k, cur) in &self.last_seen {
            let base = self.baseline.get(k).copied().unwrap_or((0, 0));
            let dd = counter_delta(base.0, cur.0);
            let du = counter_delta(base.1, cur.1);
            if dd > 0 || du > 0 {
                let e = self.persisted.entry(k.clone()).or_insert((0, 0));
                e.0 = e.0.saturating_add(dd);
                e.1 = e.1.saturating_add(du);
            }
            self.baseline.insert(k.clone(), *cur);
        }
        self.last_seen.retain(|k, _| self.present.contains(k));
        self.baseline.retain(|k, _| self.present.contains(k));
    }

    /// 落盘：先写临时文件再 rename 覆盖，保证原文件不会写一半损坏。
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, self.to_json())?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// 手动清零：已落盘累计清空，基线设为当前观测读数——显示立即
    /// 归零，之后的新增量从 0 续加（不会把清零前的运行累计再算进去）。
    /// 不影响每分钟历史记录与告警事件（那是另一个文件）。
    pub fn reset(&mut self) {
        self.persisted.clear();
        self.baseline = self.last_seen.clone();
    }

    /// 全进程历史总计：所有已知键的显示值之和——含仅存在于落盘、
    /// 本次运行未出现的进程，以及本次运行尚未折入的增量。
    /// 供快照的历史总计字段与历史窗口顶部展示。
    pub fn grand_display(&self) -> (u64, u64) {
        let mut keys: HashSet<&String> = self.persisted.keys().collect();
        keys.extend(self.last_seen.keys());
        let (mut d, mut u) = (0u64, 0u64);
        for k in keys {
            let (kd, ku) = self.display(k);
            d = d.saturating_add(kd);
            u = u.saturating_add(ku);
        }
        (d, u)
    }

    /// 已落盘总累计（只读，供界面外诊断与测试；生产路径不调用）。
    #[allow(dead_code)]
    pub fn persisted_totals(&self) -> &BTreeMap<String, (u64, u64)> {
        &self.persisted
    }

    /// 序列化为 totals.json 的文本（键按字典序，输出稳定）。
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\"version\":");
        out.push_str(&FORMAT_VERSION.to_string());
        out.push_str(",\"totals\":{");
        let mut first = true;
        for (k, (down, up)) in &self.persisted {
            if !first {
                out.push(',');
            }
            first = false;
            out.push('"');
            out.push_str(&json_escape(k));
            out.push_str("\":{\"down\":");
            out.push_str(&down.to_string());
            out.push_str(",\"up\":");
            out.push_str(&up.to_string());
            out.push('}');
        }
        out.push_str("}}\n");
        out
    }
}

impl Default for TotalsStore {
    fn default() -> Self {
        Self::new()
    }
}

/// 数据目录下的总累计文件完整路径。
pub fn totals_path(dir: &Path) -> PathBuf {
    dir.join(TOTALS_FILE_NAME)
}

/// 全进程本次总计（后端聚合求和的纯函数）：输入与聚合线程每秒的
/// 观测同构——(键, 本次运行累计下载, 本次运行累计上传)，含“系统”
/// “未归属流量”等合成行，与主进程表的全部行一一对应；输出全行
/// 之和。空输入为 (0, 0)。快照的总计字段由后端调用本函数算好，
/// GUI 只展示，保证与表格口径一致。
pub fn sum_run_totals(rows: &[(String, u64, u64)]) -> (u64, u64) {
    rows.iter().fold((0u64, 0u64), |(d, u), (_, rd, ru)| {
        (d.saturating_add(*rd), u.saturating_add(*ru))
    })
}

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

// ---------------------------------------------------------------------------
// 宽容解析：只接受本模块写出的结构；任何偏差返回 None（视为损坏）。
// ---------------------------------------------------------------------------

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            bytes: text.as_bytes(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while let Some(&b) = self.bytes.get(self.pos) {
            if b.is_ascii_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.bytes.get(self.pos).copied()
    }

    fn expect(&mut self, b: u8) -> Option<()> {
        if self.peek()? == b {
            self.pos += 1;
            Some(())
        } else {
            None
        }
    }

    fn string(&mut self) -> Option<String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let b = *self.bytes.get(self.pos)?;
            self.pos += 1;
            match b {
                b'"' => return Some(out),
                b'\\' => {
                    let e = *self.bytes.get(self.pos)?;
                    self.pos += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'u' => {
                            let hex = self.bytes.get(self.pos..self.pos + 4)?;
                            let hex = std::str::from_utf8(hex).ok()?;
                            let n = u32::from_str_radix(hex, 16).ok()?;
                            self.pos += 4;
                            out.push(char::from_u32(n).unwrap_or('\u{fffd}'));
                        }
                        _ => return None,
                    }
                }
                _ => {
                    // 多字节 UTF-8：逐字节收集后统一解码不便，按字符推进。
                    let s = std::str::from_utf8(&self.bytes[self.pos - 1..]).ok()?;
                    let c = s.chars().next()?;
                    out.push(c);
                    self.pos += c.len_utf8() - 1;
                }
            }
        }
    }

    fn number(&mut self) -> Option<u64> {
        self.skip_ws();
        let start = self.pos;
        while let Some(&b) = self.bytes.get(self.pos) {
            if b.is_ascii_digit() {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return None;
        }
        std::str::from_utf8(&self.bytes[start..self.pos])
            .ok()?
            .parse::<u64>()
            .ok()
    }

    /// 跳过任意 JSON 值（用于忽略未知字段）。
    fn skip_value(&mut self) -> Option<()> {
        match self.peek()? {
            b'"' => {
                self.string()?;
            }
            b'{' => {
                self.pos += 1;
                if self.peek()? == b'}' {
                    self.pos += 1;
                    return Some(());
                }
                loop {
                    self.string()?;
                    self.expect(b':')?;
                    self.skip_value()?;
                    match self.peek()? {
                        b',' => self.pos += 1,
                        b'}' => {
                            self.pos += 1;
                            return Some(());
                        }
                        _ => return None,
                    }
                }
            }
            b'[' => {
                self.pos += 1;
                if self.peek()? == b']' {
                    self.pos += 1;
                    return Some(());
                }
                loop {
                    self.skip_value()?;
                    match self.peek()? {
                        b',' => self.pos += 1,
                        b']' => {
                            self.pos += 1;
                            return Some(());
                        }
                        _ => return None,
                    }
                }
            }
            b't' => {
                if self.bytes[self.pos..].starts_with(b"true") {
                    self.pos += 4;
                } else {
                    return None;
                }
            }
            b'f' => {
                if self.bytes[self.pos..].starts_with(b"false") {
                    self.pos += 5;
                } else {
                    return None;
                }
            }
            b'n' => {
                if self.bytes[self.pos..].starts_with(b"null") {
                    self.pos += 4;
                } else {
                    return None;
                }
            }
            _ => {
                // 数字（含负号/小数/指数，粗略跳过）。
                let start = self.pos;
                while let Some(&b) = self.bytes.get(self.pos) {
                    if b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E') {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                if self.pos == start {
                    return None;
                }
            }
        }
        Some(())
    }

    /// 解析 `{"down":N,"up":N}`（字段顺序不限，缺失按 0，未知字段跳过）。
    fn totals_entry(&mut self) -> Option<(u64, u64)> {
        self.expect(b'{')?;
        let (mut down, mut up) = (0u64, 0u64);
        if self.peek()? == b'}' {
            self.pos += 1;
            return Some((down, up));
        }
        loop {
            let key = self.string()?;
            self.expect(b':')?;
            match key.as_str() {
                "down" => down = self.number()?,
                "up" => up = self.number()?,
                _ => self.skip_value()?,
            }
            match self.peek()? {
                b',' => self.pos += 1,
                b'}' => {
                    self.pos += 1;
                    return Some((down, up));
                }
                _ => return None,
            }
        }
    }

    /// 解析 totals 对象的全部条目。
    fn totals_map(&mut self) -> Option<BTreeMap<String, (u64, u64)>> {
        self.expect(b'{')?;
        let mut map = BTreeMap::new();
        if self.peek()? == b'}' {
            self.pos += 1;
            return Some(map);
        }
        loop {
            let key = self.string()?;
            self.expect(b':')?;
            let entry = self.totals_entry()?;
            map.insert(key, entry);
            match self.peek()? {
                b',' => self.pos += 1,
                b'}' => {
                    self.pos += 1;
                    return Some(map);
                }
                _ => return None,
            }
        }
    }
}

/// 解析 totals.json 文本，取出 persisted 映射；结构不符返回 None。
pub fn parse_persisted(text: &str) -> Option<BTreeMap<String, (u64, u64)>> {
    let mut p = Parser::new(text);
    p.expect(b'{')?;
    let mut totals: Option<BTreeMap<String, (u64, u64)>> = None;
    if p.peek()? == b'}' {
        return totals;
    }
    loop {
        let key = p.string()?;
        p.expect(b':')?;
        if key == "totals" {
            totals = Some(p.totals_map()?);
        } else {
            p.skip_value()?;
        }
        match p.peek()? {
            b',' => p.pos += 1,
            b'}' => return totals,
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(rows: &[(&str, u64, u64)]) -> Vec<(String, u64, u64)> {
        rows.iter()
            .map(|(k, d, u)| (k.to_string(), *d, *u))
            .collect()
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "throttle-totals-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn increments_accumulate_by_delta_not_by_cumulative() {
        let mut s = TotalsStore::new();
        // 第 1 分钟：运行累计涨到 (6000, 3000)。
        s.observe(&obs(&[("a.exe", 6000, 3000)]));
        // 未折入前显示就应等于增量本身。
        assert_eq!(s.display("a.exe"), (6000, 3000));
        s.fold();
        assert_eq!(s.persisted_totals()["a.exe"], (6000, 3000));
        assert_eq!(s.display("a.exe"), (6000, 3000), "折入后显示不应翻倍");
        // 第 2 分钟：运行累计涨到 (9000, 3500)，只应再加 (3000, 500)。
        s.observe(&obs(&[("a.exe", 9000, 3500)]));
        assert_eq!(s.display("a.exe"), (9000, 3500));
        s.fold();
        assert_eq!(s.persisted_totals()["a.exe"], (9000, 3500));
        // 同一读数反复 observe + fold，不得重复计入。
        s.observe(&obs(&[("a.exe", 9000, 3500)]));
        s.fold();
        s.fold();
        assert_eq!(s.persisted_totals()["a.exe"], (9000, 3500));
    }

    #[test]
    fn restart_reads_back_and_continues() {
        let dir = temp_dir("restart");
        let path = totals_path(&dir);
        // 第一次运行：累计 (5000, 2000) 后落盘退出。
        let (mut s, status) = TotalsStore::load(&path);
        assert_eq!(status, LoadStatus::Fresh);
        s.observe(&obs(&[("a.exe", 5000, 2000)]));
        s.fold();
        s.save(&path).expect("save");
        drop(s);
        // 第二次运行：读回后续加，运行累计从 0 重新长。
        let (mut s, status) = TotalsStore::load(&path);
        assert_eq!(status, LoadStatus::Loaded);
        assert_eq!(s.persisted_totals()["a.exe"], (5000, 2000));
        assert_eq!(s.display("a.exe"), (5000, 2000));
        s.observe(&obs(&[("a.exe", 700, 100)]));
        assert_eq!(s.display("a.exe"), (5700, 2100));
        s.fold();
        assert_eq!(s.persisted_totals()["a.exe"], (5700, 2100));
        s.save(&path).expect("save again");
        let (s2, status) = TotalsStore::load(&path);
        assert_eq!(status, LoadStatus::Loaded);
        assert_eq!(s2.persisted_totals()["a.exe"], (5700, 2100));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn counter_reset_within_run_counts_current_as_delta() {
        let mut s = TotalsStore::new();
        s.observe(&obs(&[("a.exe", 10_000, 0)]));
        s.fold();
        // 同路径进程重启，运行累计从 0 重长到 700：本期增量 = 700。
        s.observe(&obs(&[("a.exe", 700, 0)]));
        assert_eq!(s.display("a.exe"), (10_700, 0));
        s.fold();
        assert_eq!(s.persisted_totals()["a.exe"], (10_700, 0));
    }

    #[test]
    fn reset_zeroes_and_rebases() {
        let dir = temp_dir("reset");
        let path = totals_path(&dir);
        let (mut s, _) = TotalsStore::load(&path);
        s.observe(&obs(&[("a.exe", 5000, 2000)]));
        s.fold();
        s.save(&path).expect("save");
        // 清零：显示立即归零。
        s.reset();
        assert_eq!(s.display("a.exe"), (0, 0));
        assert!(s.persisted_totals().is_empty());
        // 分项与总计必须同时为 0（总计是分项之和，不单独存储）。
        assert_eq!(s.grand_display(), (0, 0));
        // 同一次运行内：清零前的运行累计（5000）不得被重新计入，
        // 只有其后的新增量（600/100）才算。
        s.observe(&obs(&[("a.exe", 5000, 2000)]));
        assert_eq!(s.display("a.exe"), (0, 0));
        s.observe(&obs(&[("a.exe", 5600, 2100)]));
        assert_eq!(s.display("a.exe"), (600, 100));
        s.fold();
        assert_eq!(s.persisted_totals()["a.exe"], (600, 100));
        s.save(&path).expect("save after reset");
        // 落盘读回：文件里只有清零后的累计。
        let (mut s, status) = TotalsStore::load(&path);
        assert_eq!(status, LoadStatus::Loaded);
        assert_eq!(s.persisted_totals()["a.exe"], (600, 100));
        // 应用重启后运行累计从小读数重新长：新运行的增量继续叠加。
        s.observe(&obs(&[("a.exe", 300, 50)]));
        assert_eq!(s.display("a.exe"), (900, 150));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_falls_back_to_zero() {
        let dir = temp_dir("corrupt");
        let path = totals_path(&dir);
        std::fs::write(&path, "{not json!!").unwrap();
        let (s, status) = TotalsStore::load(&path);
        assert_eq!(status, LoadStatus::Corrupted);
        assert!(s.persisted_totals().is_empty());
        // 结构看似对但数值损坏，同样判损坏。
        std::fs::write(&path, "{\"totals\":{\"a.exe\":{\"down\":\"x\",\"up\":1}}}").unwrap();
        let (s, status) = TotalsStore::load(&path);
        assert_eq!(status, LoadStatus::Corrupted);
        assert!(s.persisted_totals().is_empty());
        // 截断到一半的合法文件同样不 panic、从 0 开始。
        let mut good = TotalsStore::new();
        good.observe(&obs(&[("a.exe", 10, 20)]));
        good.fold();
        let text = good.to_json();
        std::fs::write(&path, &text[..text.len() / 2]).unwrap();
        let (s, status) = TotalsStore::load(&path);
        assert_eq!(status, LoadStatus::Corrupted);
        assert!(s.persisted_totals().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_round_trip_with_tricky_paths() {
        let mut s = TotalsStore::new();
        s.observe(&obs(&[
            ("c:\\apps\\微信 \"测试\"\\weixin.exe", 123, 456),
            ("d:\\工具\\app.exe", 7, 8),
        ]));
        s.fold();
        let text = s.to_json();
        assert!(!text.contains('\n') || text.ends_with('\n'));
        let back = parse_persisted(&text).expect("parse own output");
        assert_eq!(back.len(), 2);
        assert_eq!(back["c:\\apps\\微信 \"测试\"\\weixin.exe"], (123, 456));
        assert_eq!(back["d:\\工具\\app.exe"], (7, 8));
        // 空存储也能往返。
        let empty = TotalsStore::new();
        let back = parse_persisted(&empty.to_json()).expect("parse empty");
        assert!(back.is_empty());
    }

    #[test]
    fn sum_run_totals_covers_all_rows_including_synthetic() {
        // 多进程 + “系统”“未归属流量”合成行，全部计入。
        let rows = obs(&[
            ("c:\\apps\\a.exe", 1000, 200),
            ("c:\\apps\\b.exe", 3000, 400),
            ("system", 500, 6000),
            ("<unknown>", 50, 60),
        ]);
        assert_eq!(sum_run_totals(&rows), (4550, 6660));
        // 空表为 0。
        assert_eq!(sum_run_totals(&[]), (0, 0));
        // 单行即自身。
        assert_eq!(sum_run_totals(&obs(&[("a.exe", 7, 9)])), (7, 9));
    }

    #[test]
    fn grand_display_sums_persisted_and_pending() {
        let dir = temp_dir("grand");
        let path = totals_path(&dir);
        // a.exe 落盘 (5000, 2000) 后“退出”，本次运行不再出现。
        let (mut s, _) = TotalsStore::load(&path);
        s.observe(&obs(&[("a.exe", 5000, 2000)]));
        s.fold();
        s.save(&path).expect("save");
        drop(s);
        // 新一次运行：只有 b.exe，且其增量尚未折入落盘。
        let (mut s, status) = TotalsStore::load(&path);
        assert_eq!(status, LoadStatus::Loaded);
        s.observe(&obs(&[("b.exe", 700, 100)]));
        // 历史总计 = a 的落盘值 + b 的未折入增量。
        assert_eq!(s.grand_display(), (5700, 2100));
        s.fold();
        assert_eq!(s.grand_display(), (5700, 2100), "折入前后总计不应变化");
        // 清零后：只剩清零之后的新增量。
        s.reset();
        assert_eq!(s.grand_display(), (0, 0));
        s.observe(&obs(&[("b.exe", 750, 130)]));
        assert_eq!(s.grand_display(), (50, 30));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exited_process_final_delta_is_not_lost() {
        let mut s = TotalsStore::new();
        s.observe(&obs(&[("a.exe", 1000, 0), ("b.exe", 2000, 0)]));
        s.fold();
        // a 又传了 500 后从快照消失：最后增量仍应被折入，之后不再增长。
        s.observe(&obs(&[("a.exe", 1500, 0), ("b.exe", 2000, 0)]));
        s.observe(&obs(&[("b.exe", 2600, 0)]));
        s.fold();
        assert_eq!(s.persisted_totals()["a.exe"], (1500, 0));
        assert_eq!(s.persisted_totals()["b.exe"], (2600, 0));
        s.observe(&obs(&[("b.exe", 2600, 0)]));
        s.fold();
        assert_eq!(s.persisted_totals()["a.exe"], (1500, 0));
    }
}
