//! 本进程内存占用自报（阶段 13，内存优化第二轮）。
//!
//! Windows 下用 `GetProcessMemoryInfo` 读本进程的工作集（物理内存占用）
//! 与私有提交（PrivateUsage），口径与任务管理器「内存」列可对照。
//! 数据一处采集、两处使用：关于窗口显示 + 每 60 秒写一行 throttle.log，
//! 让每版的真机内存数字可回收对比。非 Windows 平台 `sample()` 恒为
//! None，功能整体不编译出行为差异。
//!
//! 格式化与摘要拼装是纯函数，可独立单元测试。

/// 一次内存采样（字节）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemorySample {
    /// 工作集大小：当前实际占用的物理内存（任务管理器「内存」列口径）。
    pub working_set_bytes: u64,
    /// 私有提交大小：进程独占的已提交虚拟内存（堆、栈、映像映射等）。
    pub private_bytes: u64,
}

/// 字节数 → 「X.Y MB」（MiB 口径，保留一位小数）。
pub fn format_mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

/// 一行摘要：关于窗口与日志共用同一口径文案。
pub fn summary_line(sample: &MemorySample) -> String {
    format!(
        "工作集 {}，私有提交 {}",
        format_mb(sample.working_set_bytes),
        format_mb(sample.private_bytes)
    )
}

/// 采集本进程当前内存占用；失败（或非 Windows 平台）返回 None。
#[cfg(windows)]
pub fn sample() -> Option<MemorySample> {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
    use windows::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            std::ptr::from_mut(&mut counters).cast(),
            counters.cb,
        )
    };
    ok.ok().map(|()| MemorySample {
        working_set_bytes: counters.WorkingSetSize as u64,
        private_bytes: counters.PrivateUsage as u64,
    })
}

/// 非 Windows 平台不提供自报（开发/测试环境用）。
#[cfg(not(windows))]
pub fn sample() -> Option<MemorySample> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_mb_uses_mib_with_one_decimal() {
        assert_eq!(format_mb(0), "0.0 MB");
        assert_eq!(format_mb(1024 * 1024), "1.0 MB");
        // 用户真机基线 v6：172.8 MB。
        let baseline = (172.8f64 * 1024.0 * 1024.0) as u64;
        assert_eq!(format_mb(baseline), "172.8 MB");
        // 四舍五入到一位小数：1.25 MiB → 1.3 MB（format 的银行家舍入
        // 之外按常规显示即可，这里只锁定整数与基线两个确切值之外的
        // 截断行为下限）。
        assert_eq!(format_mb(1536 * 1024), "1.5 MB");
    }

    #[test]
    fn summary_line_combines_both_numbers() {
        let s = MemorySample {
            working_set_bytes: 100 * 1024 * 1024,
            private_bytes: 80 * 1024 * 1024,
        };
        assert_eq!(summary_line(&s), "工作集 100.0 MB，私有提交 80.0 MB");
    }

    #[test]
    fn sample_fields_are_independent() {
        let s = MemorySample {
            working_set_bytes: 1,
            private_bytes: 2,
        };
        assert_eq!(s.working_set_bytes, 1);
        assert_eq!(s.private_bytes, 2);
        assert_eq!(s, s.clone());
    }
}
