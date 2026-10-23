//! 异常上传告警状态机（纯逻辑、仅依赖 std，可独立单元测试）。
//!
//! 由 aggregator 每秒以最新快照调用一次 [`AlertEngine::update`]：
//! - 某进程上传速率连续 >= 阈值达到设定的持续秒数 → 产生一次告警事件；
//! - 同一持续异常只报一次；速率回落到阈值的 80% 以下才重新布防（迟滞），
//!   避免在临界值附近反复报警；
//! - 瞬时峰值不触发：速率一旦低于阈值（未触发时），连续计时清零；
//! - 进程身份 = PID + 名称 + 路径；身份变化（典型是 PID 被复用）会重置
//!   该进程的状态，避免把两个进程的流量串在一起；
//! - 不在当前快照中的进程，其状态会被清除（进程已退出）；
//! - 阈值为 0 表示关闭告警。
//!
//! 告警只陈述"持续高上传"这一事实，不对程序行为做任何定性判断。

use std::collections::HashMap;

/// 告警判断所需的每进程采样（由后端快照转换而来）。
#[derive(Debug, Clone, PartialEq)]
pub struct ProcSample {
    /// 快照中的键（通常是小写 exe 路径）。
    pub key: String,
    pub pid: u32,
    pub name: String,
    pub exe_path: String,
    /// 当前上传速率（字节/秒）。
    pub up_rate: u64,
}

/// 一次触发的告警事件。
#[derive(Debug, Clone, PartialEq)]
pub struct AlertEvent {
    pub key: String,
    pub pid: u32,
    pub name: String,
    pub exe_path: String,
    /// 触发瞬间观测到的上传速率（字节/秒）。
    pub up_rate: u64,
    /// 触发时生效的阈值（字节/秒）。
    pub threshold_bps: u64,
    /// 已持续秒数（触发 tick − 首次达标 tick）。
    pub sustained_secs: u64,
}

/// 当前仍处于"已触发且未恢复"状态的进程信息，供界面横幅展示。
#[derive(Debug, Clone, PartialEq)]
pub struct FiredInfo {
    pub key: String,
    pub pid: u32,
    pub name: String,
    pub exe_path: String,
    pub threshold_bps: u64,
    /// 首次持续达标的 tick（秒序号）。
    pub above_since: u64,
    /// 触发时的 tick（秒序号）。
    pub fired_tick: u64,
}

#[derive(Debug, Clone)]
struct ProcState {
    identity: String,
    pid: u32,
    name: String,
    exe_path: String,
    above_since: Option<u64>,
    fired_tick: Option<u64>,
    threshold_at_fire: u64,
}

/// 告警引擎。tick 是调用方维护的单调递增秒序号（从 1 开始即可）。
pub struct AlertEngine {
    threshold_bps: u64,
    duration_secs: u64,
    /// 归一化后的忽略名单（小写、去空白）。
    ignore: Vec<String>,
    states: HashMap<String, ProcState>,
}

fn identity_of(s: &ProcSample) -> String {
    format!(
        "{}\u{1}{}\u{1}{}",
        s.pid,
        s.name.to_lowercase(),
        s.exe_path.to_lowercase()
    )
}

fn normalize_ignore(ignore: &[String]) -> Vec<String> {
    ignore
        .iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn is_ignored(ignore: &[String], s: &ProcSample) -> bool {
    if ignore.is_empty() {
        return false;
    }
    let name = s.name.to_lowercase();
    let path = s.exe_path.to_lowercase();
    let key = s.key.to_lowercase();
    ignore.iter().any(|e| e == &name || e == &path || e == &key)
}

impl AlertEngine {
    pub fn new(threshold_bps: u64, duration_secs: u64, ignore: Vec<String>) -> Self {
        Self {
            threshold_bps,
            duration_secs,
            ignore: normalize_ignore(&ignore),
            states: HashMap::new(),
        }
    }

    /// 更新配置。配置变化后清空全部状态，避免旧阈值下的计时/触发残留
    /// 产生误解；之后按新配置重新累计与布防。
    pub fn set_config(&mut self, threshold_bps: u64, duration_secs: u64, ignore: Vec<String>) {
        self.threshold_bps = threshold_bps;
        self.duration_secs = duration_secs;
        self.ignore = normalize_ignore(&ignore);
        self.states.clear();
    }

    /// 以一秒的快照推进状态机，返回本 tick 新触发的告警事件（通常为空）。
    pub fn update(&mut self, tick: u64, samples: &[ProcSample]) -> Vec<AlertEvent> {
        let mut events = Vec::new();
        if self.threshold_bps == 0 {
            self.states.clear();
            return events;
        }
        // 重新布防线：阈值的 80%（整数运算，先乘后除减少误差）。
        let rearm = self.threshold_bps / 5 * 4;

        // 清除已退出进程的状态。
        self.states
            .retain(|k, _| samples.iter().any(|s| &s.key == k));

        for s in samples {
            if is_ignored(&self.ignore, s) {
                self.states.remove(&s.key);
                continue;
            }
            let identity = identity_of(s);
            let st = self
                .states
                .entry(s.key.clone())
                .or_insert_with(|| ProcState {
                    identity: identity.clone(),
                    pid: s.pid,
                    name: s.name.clone(),
                    exe_path: s.exe_path.clone(),
                    above_since: None,
                    fired_tick: None,
                    threshold_at_fire: 0,
                });
            if st.identity != identity {
                // PID 复用或进程被替换：按新进程重新计时。
                st.identity = identity;
                st.above_since = None;
                st.fired_tick = None;
            }
            st.pid = s.pid;
            st.name = s.name.clone();
            st.exe_path = s.exe_path.clone();

            if s.up_rate >= self.threshold_bps {
                if st.above_since.is_none() {
                    st.above_since = Some(tick);
                }
                let since = st.above_since.unwrap_or(tick);
                if st.fired_tick.is_none() && tick.saturating_sub(since) >= self.duration_secs {
                    st.fired_tick = Some(tick);
                    st.threshold_at_fire = self.threshold_bps;
                    events.push(AlertEvent {
                        key: s.key.clone(),
                        pid: s.pid,
                        name: s.name.clone(),
                        exe_path: s.exe_path.clone(),
                        up_rate: s.up_rate,
                        threshold_bps: self.threshold_bps,
                        sustained_secs: tick.saturating_sub(since),
                    });
                }
            } else if s.up_rate < rearm {
                // 回落到布防线以下：恢复正常，可再次触发。
                st.above_since = None;
                st.fired_tick = None;
            } else if st.fired_tick.is_none() {
                // 迟滞区内且尚未触发：连续达标被打断，计时清零；
                // 已触发的保持触发状态，直到回落到布防线以下。
                st.above_since = None;
            }
        }
        events
    }

    /// 当前处于已触发状态的全部进程（供横幅展示，取最近触发的一个即可）。
    pub fn fired(&self) -> Vec<FiredInfo> {
        self.states
            .iter()
            .filter_map(|(key, st)| {
                st.fired_tick.map(|fired_tick| FiredInfo {
                    key: key.clone(),
                    pid: st.pid,
                    name: st.name.clone(),
                    exe_path: st.exe_path.clone(),
                    threshold_bps: st.threshold_at_fire,
                    above_since: st.above_since.unwrap_or(fired_tick),
                    fired_tick,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(key: &str, pid: u32, name: &str, up_rate: u64) -> ProcSample {
        ProcSample {
            key: key.to_string(),
            pid,
            name: name.to_string(),
            exe_path: key.to_string(),
            up_rate,
        }
    }

    fn engine() -> AlertEngine {
        // 阈值 100 B/s，持续 3 秒。
        AlertEngine::new(100, 3, Vec::new())
    }

    #[test]
    fn fires_only_after_sustained_above_threshold() {
        let mut e = engine();
        let s = sample("a.exe", 10, "a.exe", 150);
        assert!(e.update(1, std::slice::from_ref(&s)).is_empty());
        assert!(e.update(2, std::slice::from_ref(&s)).is_empty());
        assert!(e.update(3, std::slice::from_ref(&s)).is_empty());
        let ev = e.update(4, &[s]);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].sustained_secs, 3);
        assert_eq!(ev[0].threshold_bps, 100);
        assert_eq!(e.fired().len(), 1);
    }

    #[test]
    fn transient_spike_does_not_fire() {
        let mut e = engine();
        let hi = sample("a.exe", 10, "a.exe", 500);
        let lo = sample("a.exe", 10, "a.exe", 0);
        // 两次 1 秒峰值，中间回落：不触发。
        assert!(e.update(1, std::slice::from_ref(&hi)).is_empty());
        assert!(e.update(2, &[lo]).is_empty());
        assert!(e.update(3, &[hi]).is_empty());
        assert!(e.fired().is_empty());
    }

    #[test]
    fn fires_once_per_episode_then_rearms_below_80_percent() {
        let mut e = engine();
        let hi = sample("a.exe", 10, "a.exe", 150);
        for t in 1..=4 {
            e.update(t, std::slice::from_ref(&hi));
        }
        assert_eq!(e.fired().len(), 1);
        // 持续超限：不再重复触发。
        assert!(e.update(5, std::slice::from_ref(&hi)).is_empty());
        assert!(e.update(6, std::slice::from_ref(&hi)).is_empty());
        // 回落到迟滞区（85，介于 80 与 100 之间）：仍保持已触发，不重复报。
        let mid = sample("a.exe", 10, "a.exe", 85);
        assert!(e.update(7, &[mid]).is_empty());
        assert_eq!(e.fired().len(), 1);
        // 回落到布防线以下（79 < 80）：恢复，重新布防。
        let lo = sample("a.exe", 10, "a.exe", 79);
        assert!(e.update(8, &[lo]).is_empty());
        assert!(e.fired().is_empty());
        // 再次持续超限：应再次触发。
        assert!(e.update(9, std::slice::from_ref(&hi)).is_empty());
        assert!(e.update(10, std::slice::from_ref(&hi)).is_empty());
        assert!(e.update(11, std::slice::from_ref(&hi)).is_empty());
        let ev = e.update(12, &[hi]);
        assert_eq!(ev.len(), 1);
    }

    #[test]
    fn dip_into_hysteresis_band_breaks_continuity_before_firing() {
        let mut e = engine();
        let hi = sample("a.exe", 10, "a.exe", 150);
        let mid = sample("a.exe", 10, "a.exe", 90);
        e.update(1, std::slice::from_ref(&hi));
        e.update(2, std::slice::from_ref(&hi));
        // 第 3 秒掉进迟滞区：连续计时清零，不触发。
        assert!(e.update(3, &[mid]).is_empty());
        e.update(4, std::slice::from_ref(&hi));
        e.update(5, std::slice::from_ref(&hi));
        assert!(e.update(6, std::slice::from_ref(&hi)).is_empty());
        let ev = e.update(7, &[hi]);
        assert_eq!(ev.len(), 1);
    }

    #[test]
    fn ignored_processes_never_fire() {
        let mut e = AlertEngine::new(100, 3, vec!["Trusted.EXE".to_string()]);
        let s = sample("c:\\apps\\trusted.exe", 10, "trusted.exe", 9999);
        for t in 1..=10 {
            assert!(e.update(t, std::slice::from_ref(&s)).is_empty());
        }
        assert!(e.fired().is_empty());
        // 按完整路径忽略同样有效。
        let mut e2 = AlertEngine::new(100, 3, vec!["c:\\apps\\trusted.exe".to_string()]);
        for t in 1..=10 {
            assert!(e2.update(t, std::slice::from_ref(&s)).is_empty());
        }
    }

    #[test]
    fn pid_reuse_resets_timer() {
        let mut e = engine();
        let first = sample("a.exe", 10, "a.exe", 150);
        e.update(1, &[first]);
        e.update(2, &[sample("a.exe", 10, "a.exe", 150)]);
        // 同一路径但 PID 变了（旧进程退出、新进程复用路径）：重新计时。
        let second = sample("a.exe", 77, "a.exe", 150);
        assert!(e.update(3, std::slice::from_ref(&second)).is_empty());
        assert!(e.update(4, std::slice::from_ref(&second)).is_empty());
        assert!(e.update(5, std::slice::from_ref(&second)).is_empty());
        let ev = e.update(6, &[second]);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].pid, 77);
    }

    #[test]
    fn exited_process_state_is_cleared() {
        let mut e = engine();
        let s = sample("a.exe", 10, "a.exe", 150);
        for t in 1..=4 {
            e.update(t, std::slice::from_ref(&s));
        }
        assert_eq!(e.fired().len(), 1);
        // 进程退出（不在快照中）：状态清除。
        e.update(5, &[]);
        assert!(e.fired().is_empty());
    }

    #[test]
    fn zero_threshold_disables_alerts() {
        let mut e = AlertEngine::new(0, 3, Vec::new());
        let s = sample("a.exe", 10, "a.exe", u64::MAX / 2);
        for t in 1..=10 {
            assert!(e.update(t, std::slice::from_ref(&s)).is_empty());
        }
        assert!(e.fired().is_empty());
    }

    #[test]
    fn config_change_clears_state() {
        let mut e = engine();
        let s = sample("a.exe", 10, "a.exe", 150);
        for t in 1..=4 {
            e.update(t, std::slice::from_ref(&s));
        }
        assert_eq!(e.fired().len(), 1);
        e.set_config(200, 3, Vec::new());
        assert!(e.fired().is_empty());
        // 新阈值 200：150 不再达标。
        assert!(e.update(5, &[s]).is_empty());
    }
}
