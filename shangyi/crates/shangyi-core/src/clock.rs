//! 时钟 seam。
//!
//! # 为什么时钟是一个可注入的 trait 而不是直接调 `SystemTime::now()`
//!
//! 时间戳会**进入事件日志**，而事件日志是审计的权威（§8.1 事件溯源）。
//! 若日志写入处直接读系统时钟，那么：
//!
//! 1. **测试无法断言日志内容**——每次运行时间戳都不同，只能断言"有个时间戳"，
//!    测不出"时间戳取自真实事件而非补丁"这类要求。
//! 2. **崩溃修复的确定性会被破坏**（§7.1）：[`crate::session::interrupted_turn_closers`]
//!    的契约是"时间戳复用最后一个真实事件"，若时间源不可控，就无法验证它真的
//!    没有偷偷用当前时间。
//!
//! 故：生产用 [`SystemClock`]，测试用 [`SeqClock`]，两者都只满足这一个接口。

/// 提供 ISO-8601（UTC）时间戳。
pub trait Clock: Send + Sync {
    /// 当前时刻，形如 `2026-09-17T08:30:00Z`。
    fn now_iso8601(&self) -> String;
}

/// 系统时钟（生产用）。
///
/// 格式固定为 UTC 且到秒——**不带亚秒**：事件的先后由日志序号
/// （[`crate::case_log::Recorded::seq`]）保证，时间戳只表达"现实世界何时"，
/// 混入亚秒只会让同一秒内的事件看起来有精确次序，而那不成立。
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_iso8601(&self) -> String {
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => epoch_secs_to_iso8601(d.as_secs() as i64),
            // 时钟早于纪元（系统时钟未同步）：显式表达，不假装是 1970。
            // 审计看到 "unknown" 就知道要查时钟，看到 "1970-01-01T00:00:00Z"
            // 只会以为数据是对的。
            Err(_) => "unknown".to_string(),
        }
    }
}

/// 确定性时钟：返回 `seq:<n>` 并自增（测试用）。
///
/// **它刻意不返回看起来像时间的东西**：若它返回 `2026-01-01T00:00:00Z`，
/// 一个误用生产路径的测试会通过，而"时间戳其实来自假时钟"这件事无从察觉。
#[derive(Debug, Default)]
pub struct SeqClock {
    n: std::sync::atomic::AtomicU64,
}

impl SeqClock {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Clock for SeqClock {
    fn now_iso8601(&self) -> String {
        let n = self
            .n
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("seq:{n}")
    }
}

/// 待判定时刻：固定返回同一串（测试"同一时刻的多条事件"用）。
#[derive(Debug, Clone)]
pub struct FixedClock(pub String);

impl Clock for FixedClock {
    fn now_iso8601(&self) -> String {
        self.0.clone()
    }
}

/// 纪元秒 → ISO-8601（UTC）。
///
/// # 为什么手写而不是引入日期库
///
/// 这里只需要**一个方向**的转换（epoch → 固定格式字符串），不需要时区、
/// 不需要解析、不需要日历运算（闰秒由天数累加自然处理）。
/// 为它引入 `chrono`（或 `time` 的 formatting feature）会把一个 20 行的纯函数
/// 换成一个依赖树——而商奕对依赖的默认态度是"能用确定性小函数解决就不引依赖"。
///
/// 正确性由 [`tests::epoch_conversion_matches_known_dates`] 用**已知日期**钉住
/// （含 2000-02-29 闰日、2100 非闰年等边界）。
pub fn epoch_secs_to_iso8601(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// 天数（自 1970-01-01）→ (年, 月, 日)。Howard Hinnant 的 `civil_from_days`。
///
/// 选它的理由：它是**已知正确**的整数算法（被广泛复用与验证），
/// 且在 1970–2100 全范围内无分支特例。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_conversion_matches_known_dates() {
        // 已知值（UTC），覆盖闰年、世纪、非闰世纪三类边界
        assert_eq!(epoch_secs_to_iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(epoch_secs_to_iso8601(1), "1970-01-01T00:00:01Z");
        assert_eq!(epoch_secs_to_iso8601(86_399), "1970-01-01T23:59:59Z");
        assert_eq!(epoch_secs_to_iso8601(86_400), "1970-01-02T00:00:00Z");
        // 2000-02-29 是闰日（2000 能被 400 整除）
        assert_eq!(epoch_secs_to_iso8601(951_782_400), "2000-02-29T00:00:00Z");
        // 2026-09-17（本项目的开发日）——注意：非闰年
        assert_eq!(epoch_secs_to_iso8601(1_789_603_200), "2026-09-17T00:00:00Z");
        // 2100-03-01：2100 能被 100 整除但不能被 400 整除 ⇒ **非**闰年，
        // 故 2100-02-29 不存在。这条是"世纪非闰年"的边界。
        assert_eq!(epoch_secs_to_iso8601(4_107_542_400), "2100-03-01T00:00:00Z");
        // 纪元前的负秒也必须给出合法日期（div_euclid 保证向负无穷取整）
        assert_eq!(epoch_secs_to_iso8601(-1), "1969-12-31T23:59:59Z");
        assert_eq!(epoch_secs_to_iso8601(-86_400), "1969-12-31T00:00:00Z");
    }

    #[test]
    fn system_clock_produces_parseable_shape() {
        let s = SystemClock.now_iso8601();
        if s == "unknown" {
            // 系统时钟早于纪元：显式跳过而非断言失败。
            // 此时 clocks 本身有问题，不是本函数的问题。
            return;
        }
        assert_eq!(s.len(), 20, "长度应为 20（YYYY-MM-DDTHH:MM:SSZ）：{s}");
        assert!(s.ends_with('Z'), "必须标注 UTC：{s}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
        assert_eq!(&s[19..20], "Z");
        // 年份应为合理值（不是 1970 的假值被当成真值用）
        let y: i32 = s[..4].parse().expect("年份可解析");
        assert!(y >= 2020, "系统时钟年份可疑：{s}");
    }

    #[test]
    fn seq_clock_is_deterministic_and_not_time_shaped() {
        let c = SeqClock::new();
        assert_eq!(c.now_iso8601(), "seq:0");
        assert_eq!(c.now_iso8601(), "seq:1");
        // 关键：它**不**产出看起来像时间的字符串
        let s = c.now_iso8601();
        assert!(!s.contains('T') || !s.ends_with('Z'), "假时钟不得伪装成真实时间：{s}");
    }

    #[test]
    fn fixed_clock_repeats() {
        let c = FixedClock("2026-09-17T00:00:00Z".into());
        assert_eq!(c.now_iso8601(), c.now_iso8601());
    }
}
