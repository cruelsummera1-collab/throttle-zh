//! 界面重绘节律的纯决策（阶段 13，内存优化第二轮）。
//!
//! 背景：快照约 1 秒一帧，旧实现可见时固定 500 ms 无条件排下一帧，
//! 空闲也保持 2 fps 的全界面镶嵌/纹理周转。这里把「下一帧最迟多久」
//! 的决策抽成纯函数：
//! - 窗口隐藏在托盘里：1000 ms 低频轮询（托盘动作、唤起对齐靠它）；
//! - 本帧有用户输入（键鼠事件/按键按下/指针移动）：100 ms，
//!   让交互尾随的动画与状态变化跟手；
//! - 可见但空闲：1000 ms，与快照节拍对齐——新快照最迟约 1 秒上屏，
//!   对 30 秒量级的告警横幅无感知影响（系统弹窗在后端线程，不经界面）。
//!
//! egui 内部动画（开关过渡、光标闪烁等）自带更短的重绘请求，
//! request_repaint_after 取所有请求的最早者，本决策只决定上限。

/// 下一帧重绘的最迟延迟（毫秒）。
pub fn repaint_delay_ms(window_hidden: bool, had_input: bool) -> u64 {
    if window_hidden {
        1000
    } else if had_input {
        100
    } else {
        1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_window_polls_slowly() {
        assert_eq!(repaint_delay_ms(true, false), 1000);
        // 隐藏时即使误报输入也保持低频：托盘状态没有可见界面要跟手。
        assert_eq!(repaint_delay_ms(true, true), 1000);
    }

    #[test]
    fn visible_with_input_is_snappy() {
        assert_eq!(repaint_delay_ms(false, true), 100);
    }

    #[test]
    fn visible_idle_follows_snapshot_cadence() {
        assert_eq!(repaint_delay_ms(false, false), 1000);
    }
}
