//! Rule persistence: load/save `HashMap<String, Rule>` as JSON at
//! `%APPDATA%\Throttle\rules.json`.
//!
//! Rules are keyed by lowercase executable path (the same key the flow table
//! and GUI use). A missing file is treated as an empty rule set; parse errors
//! are logged and also fall back to empty so a corrupt file never crashes the
//! backend.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::types::CaptureMode;
use crate::types::Rule;

/// A rule is "effective" if it actually changes traffic: it blocks the
/// process or limits at least one direction. A rule with both directions
/// unlimited and not blocked is inert — it must not force intercept mode.
/// Note `Some(0)` counts as effective: an explicit zero limit blocks that
/// direction (see the engine's verdict logic).
pub fn is_effective(rule: &Rule) -> bool {
    rule.blocked || rule.down_limit.is_some() || rule.up_limit.is_some()
}

/// Desired capture mode for a whole rule set: Divert if ANY rule is
/// effective, Sniff otherwise. This is deliberately evaluated over ALL
/// applications' rules, not just one (e.g. the speed-test browser): a single
/// effective rule anywhere keeps the whole capture path in intercept mode,
/// because there is only one network-layer handle.
pub fn desired_mode(rules: &HashMap<String, Rule>) -> CaptureMode {
    if rules.values().any(is_effective) {
        CaptureMode::Divert
    } else {
        CaptureMode::Sniff
    }
}

/// Directory that holds our persisted state: `%APPDATA%\Throttle`.
pub fn config_dir() -> PathBuf {
    // APPDATA is always set for interactive users on Windows. If it is somehow
    // absent (unusual service contexts), fall back to the current directory so
    // we still have somewhere to write.
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Throttle")
}

/// Full path to the rules file.
pub fn rules_path() -> PathBuf {
    config_dir().join("rules.json")
}

/// Load the persisted rules. Missing file or any error => empty map.
pub fn load() -> HashMap<String, Rule> {
    let path = rules_path();
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("no rules file at {}, starting empty", path.display());
            return HashMap::new();
        }
        Err(e) => {
            tracing::warn!("failed to read {}: {e}", path.display());
            return HashMap::new();
        }
    };

    match serde_json::from_str::<HashMap<String, Rule>>(&data) {
        Ok(mut map) => {
            // Defensively normalise keys to lowercase so lookups from the engine
            // (which always lowercases) match regardless of how the file was written.
            map = map
                .into_iter()
                .map(|(k, mut v)| {
                    let key = k.to_lowercase();
                    v.exe_path = v.exe_path.to_lowercase();
                    (key, v)
                })
                .collect();
            tracing::info!("loaded {} rule(s) from {}", map.len(), path.display());
            map
        }
        Err(e) => {
            tracing::warn!("failed to parse {}: {e}; starting empty", path.display());
            HashMap::new()
        }
    }
}

/// Persist the given rules as pretty JSON, creating the directory if needed.
pub fn save(rules: &HashMap<String, Rule>) -> anyhow::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let path = rules_path();
    let json = serde_json::to_string_pretty(rules)?;
    std::fs::write(&path, json)?;
    tracing::debug!("saved {} rule(s) to {}", rules.len(), path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// 告警设置持久化（与 rules 相同的容错模式）：%APPDATA%\Throttle\settings.json
// ---------------------------------------------------------------------------

use crate::types::AlertSettings;

/// Full path to the alert-settings file.
pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

/// Load the persisted alert settings. Missing/corrupt file => defaults.
pub fn load_settings() -> AlertSettings {
    let path = settings_path();
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("no settings file at {}, using defaults", path.display());
            return AlertSettings::default();
        }
        Err(e) => {
            tracing::warn!("failed to read {}: {e}", path.display());
            return AlertSettings::default();
        }
    };
    match serde_json::from_str::<AlertSettings>(&data) {
        Ok(mut s) => {
            // 归一化忽略名单，避免大小写/空白导致匹配失效。
            s.ignore = s
                .ignore
                .iter()
                .map(|e| e.trim().to_lowercase())
                .filter(|e| !e.is_empty())
                .collect();
            tracing::info!("loaded alert settings from {}", path.display());
            s
        }
        Err(e) => {
            tracing::warn!("failed to parse {}: {e}; using defaults", path.display());
            AlertSettings::default()
        }
    }
}

/// Persist the alert settings as pretty JSON, creating the directory if needed.
pub fn save_settings(settings: &AlertSettings) -> anyhow::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let path = settings_path();
    let json = serde_json::to_string_pretty(settings)?;
    std::fs::write(&path, json)?;
    tracing::debug!("saved alert settings to {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_round_trip_through_json() {
        let mut rules: HashMap<String, Rule> = HashMap::new();
        rules.insert(
            "c:\\program files\\steam\\steam.exe".to_string(),
            Rule {
                exe_path: "c:\\program files\\steam\\steam.exe".to_string(),
                down_limit: Some(2 * 1024 * 1024),
                up_limit: None,
                blocked: false,
            },
        );
        rules.insert(
            "c:\\windows\\system32\\curl.exe".to_string(),
            Rule {
                exe_path: "c:\\windows\\system32\\curl.exe".to_string(),
                down_limit: None,
                up_limit: Some(512 * 1024),
                blocked: true,
            },
        );

        let json = serde_json::to_string_pretty(&rules).expect("serialize");
        let back: HashMap<String, Rule> = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(rules, back);
    }

    #[test]
    fn empty_map_round_trips() {
        let rules: HashMap<String, Rule> = HashMap::new();
        let json = serde_json::to_string(&rules).unwrap();
        let back: HashMap<String, Rule> = serde_json::from_str(&json).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn rule_defaults_are_unlimited_and_unblocked() {
        let r = Rule::default();
        assert!(r.down_limit.is_none());
        assert!(r.up_limit.is_none());
        assert!(!r.blocked);
    }

    #[test]
    fn effective_rule_predicate_matrix() {
        // Blocked counts even with no limits.
        let blocked = Rule {
            blocked: true,
            ..Rule::default()
        };
        assert!(is_effective(&blocked));
        // Either direction limited counts.
        let down_only = Rule {
            down_limit: Some(1024),
            ..Rule::default()
        };
        assert!(is_effective(&down_only));
        let up_only = Rule {
            up_limit: Some(1024),
            ..Rule::default()
        };
        assert!(is_effective(&up_only));
        // An explicit zero limit blocks that direction, so it counts.
        let zero_down = Rule {
            down_limit: Some(0),
            ..Rule::default()
        };
        assert!(is_effective(&zero_down));
        // Both directions unlimited and not blocked: inert.
        assert!(!is_effective(&Rule::default()));
    }

    #[test]
    fn desired_mode_checks_all_rules_not_just_one() {
        // Empty set: sniff.
        let empty: HashMap<String, Rule> = HashMap::new();
        assert_eq!(desired_mode(&empty), CaptureMode::Sniff);
        // Only inert rules: still sniff (an inert rule must not force divert).
        let mut inert_only: HashMap<String, Rule> = HashMap::new();
        inert_only.insert("c:\\a.exe".to_string(), Rule::default());
        inert_only.insert("c:\\b.exe".to_string(), Rule::default());
        assert_eq!(desired_mode(&inert_only), CaptureMode::Sniff);
        // One effective rule among many inert ones: divert. This is the
        // multi-application case — the check must not stop at the first rule.
        let mut mixed: HashMap<String, Rule> = HashMap::new();
        mixed.insert("c:\\a.exe".to_string(), Rule::default());
        mixed.insert(
            "c:\\b.exe".to_string(),
            Rule {
                up_limit: Some(512 * 1024),
                ..Rule::default()
            },
        );
        mixed.insert("c:\\c.exe".to_string(), Rule::default());
        assert_eq!(desired_mode(&mixed), CaptureMode::Divert);
    }

    #[test]
    fn alert_settings_round_trip_and_defaults() {
        // 默认值：500 KB/s、30 秒、忽略名单为空。
        let d = AlertSettings::default();
        assert_eq!(d.up_threshold_bps, 500 * 1024);
        assert_eq!(d.duration_secs, 30);
        assert!(d.ignore.is_empty());

        let s = AlertSettings {
            up_threshold_bps: 1024,
            duration_secs: 45,
            ignore: vec!["a.exe".to_string(), "c:\\b.exe".to_string()],
        };
        let json = serde_json::to_string_pretty(&s).unwrap();
        let back: AlertSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
