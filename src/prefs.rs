//! 界面偏好（纯 GUI 侧设置，与后端无关）的本地持久化。
//!
//! 存 `%APPDATA%\Throttle\gui-prefs.json`。目前只有两项：
//! 「关闭窗口时缩到托盘」开关（默认开启，用户明确要的功能）与
//! 「托盘首次提示已发过」标记。文件不存在 = 用默认值；文件损坏 =
//! 记状态、用默认值，绝不 panic、绝不影响监控主流程。
//!
//! JSON 手写读写（与 history.rs / totals.rs 同一套路）：字段只有
//! 两个布尔值，按字段独立解析——缺哪个字段就用哪个字段的默认值，
//! 以后加字段也不会让老文件失效。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 界面偏好。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuiPrefs {
    /// 关闭窗口时缩到系统托盘（后台继续监控）。默认开启。
    pub minimize_to_tray: bool,
    /// 「已缩到托盘」首次提示是否已发过（只提示一次）。
    pub tray_hint_shown: bool,
}

impl Default for GuiPrefs {
    fn default() -> Self {
        GuiPrefs {
            minimize_to_tray: true,
            tray_hint_shown: false,
        }
    }
}

/// 加载结果：区分「文件不存在」与「文件损坏」，供调用方决定日志。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadStatus {
    Fresh,
    Loaded,
    Corrupted,
}

/// 偏好存储：负责 load/save，读写失败都不上抛 panic。
pub struct PrefsStore {
    path: PathBuf,
    prefs: GuiPrefs,
}

impl PrefsStore {
    /// 从 `path` 加载。文件不存在 → 默认值 + Fresh；内容损坏 →
    /// 默认值 + Corrupted（调用方记一条日志即可）。
    pub fn load(path: PathBuf) -> (PrefsStore, LoadStatus) {
        match std::fs::read_to_string(&path) {
            Ok(text) => match parse_prefs(&text) {
                Some(prefs) => (PrefsStore { path, prefs }, LoadStatus::Loaded),
                None => (
                    PrefsStore {
                        path,
                        prefs: GuiPrefs::default(),
                    },
                    LoadStatus::Corrupted,
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
                PrefsStore {
                    path,
                    prefs: GuiPrefs::default(),
                },
                LoadStatus::Fresh,
            ),
            Err(_) => (
                PrefsStore {
                    path,
                    prefs: GuiPrefs::default(),
                },
                LoadStatus::Corrupted,
            ),
        }
    }

    pub fn prefs(&self) -> &GuiPrefs {
        &self.prefs
    }

    /// 修改并立即落盘（先写临时文件再改名，与 totals.rs 同一手法，
    /// 崩溃时最多丢这一次修改，不会写坏旧文件）。
    pub fn update(&mut self, prefs: GuiPrefs) -> std::io::Result<()> {
        self.prefs = prefs;
        self.save()
    }

    fn save(&self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serialize_prefs(&self.prefs))?;
        std::fs::rename(&tmp, &self.path)
    }
}

/// 序列化为单行 JSON。
fn serialize_prefs(prefs: &GuiPrefs) -> String {
    format!(
        "{{\"minimize_to_tray\":{},\"tray_hint_shown\":{}}}\n",
        prefs.minimize_to_tray, prefs.tray_hint_shown
    )
}

/// 解析：粗校验是 JSON 对象后逐字段取布尔值，缺字段用该字段默认值；
/// 整体不像对象或字段值非布尔时返回 None（损坏）。
fn parse_prefs(text: &str) -> Option<GuiPrefs> {
    let trimmed = text.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return None;
    }
    let defaults = GuiPrefs::default();
    let mut fields: BTreeMap<&str, bool> = BTreeMap::new();
    for key in ["minimize_to_tray", "tray_hint_shown"] {
        if let Some(v) = find_bool(trimmed, key)? {
            fields.insert(key, v);
        }
    }
    Some(GuiPrefs {
        minimize_to_tray: *fields
            .get("minimize_to_tray")
            .unwrap_or(&defaults.minimize_to_tray),
        tray_hint_shown: *fields
            .get("tray_hint_shown")
            .unwrap_or(&defaults.tray_hint_shown),
    })
}

/// 在 JSON 文本里找 `"key": true/false`。键不存在返回 Ok(None)；
/// 键存在但值不是布尔字面量返回 Err（视为损坏）。
fn find_bool(text: &str, key: &str) -> Option<Option<bool>> {
    let needle = format!("\"{key}\"");
    let Some(pos) = text.find(&needle) else {
        return Some(None);
    };
    let rest = text[pos + needle.len()..].trim_start();
    let Some(rest) = rest.strip_prefix(':') else {
        return None;
    };
    let rest = rest.trim_start();
    if let Some(t) = rest.strip_prefix("true") {
        if value_boundary_ok(t) {
            return Some(Some(true));
        }
        return None;
    }
    if let Some(t) = rest.strip_prefix("false") {
        if value_boundary_ok(t) {
            return Some(Some(false));
        }
        return None;
    }
    None
}

/// 布尔字面量后面必须是分隔符（空白、逗号、右括号），避免把
/// `trueX` 之类内容误认成布尔值。
fn value_boundary_ok(rest: &str) -> bool {
    match rest.chars().next() {
        None => true,
        Some(c) => c.is_whitespace() || c == ',' || c == '}',
    }
}

/// 偏好文件路径（数据目录由调用方按既有口径给出）。
pub fn prefs_path(config_dir: &Path) -> PathBuf {
    config_dir.join("gui-prefs.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "throttle-prefs-test-{}-{tag}.json",
            std::process::id()
        ))
    }

    #[test]
    fn defaults_have_tray_enabled_and_hint_pending() {
        let d = GuiPrefs::default();
        assert!(d.minimize_to_tray);
        assert!(!d.tray_hint_shown);
    }

    #[test]
    fn missing_file_loads_defaults() {
        let path = temp_path("missing");
        let _ = std::fs::remove_file(&path);
        let (store, status) = PrefsStore::load(path);
        assert_eq!(status, LoadStatus::Fresh);
        assert_eq!(store.prefs(), &GuiPrefs::default());
    }

    #[test]
    fn save_load_roundtrip() {
        let path = temp_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        let (mut store, _) = PrefsStore::load(path.clone());
        store
            .update(GuiPrefs {
                minimize_to_tray: false,
                tray_hint_shown: true,
            })
            .unwrap();
        let (reloaded, status) = PrefsStore::load(path.clone());
        assert_eq!(status, LoadStatus::Loaded);
        assert_eq!(
            reloaded.prefs(),
            &GuiPrefs {
                minimize_to_tray: false,
                tray_hint_shown: true,
            }
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupted_file_falls_back_to_defaults() {
        let path = temp_path("corrupt");
        std::fs::write(&path, "this is not json").unwrap();
        let (store, status) = PrefsStore::load(path.clone());
        assert_eq!(status, LoadStatus::Corrupted);
        assert_eq!(store.prefs(), &GuiPrefs::default());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        assert_eq!(
            parse_prefs("{\"minimize_to_tray\":false}"),
            Some(GuiPrefs {
                minimize_to_tray: false,
                tray_hint_shown: false,
            })
        );
        assert_eq!(
            parse_prefs("{\"tray_hint_shown\":true}"),
            Some(GuiPrefs {
                minimize_to_tray: true,
                tray_hint_shown: true,
            })
        );
        // 值不是布尔 → 损坏；整体不是对象 → 损坏。
        assert_eq!(parse_prefs("{\"minimize_to_tray\":1}"), None);
        assert_eq!(parse_prefs("[1,2]"), None);
        assert_eq!(parse_prefs("{\"minimize_to_tray\":trueX}"), None);
    }
}
