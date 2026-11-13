//! 一次性回放：把一次真实咨询里**全部 61 条失败入参**重新喂给归一化器，
//! 看修复后有多少条能被接受。
//!
//! 这不是常规回归测试（依赖一份导出的样本文件），而是**修复效果的证据**。
//! 样本来自 `case-20260919-scenario-063033`（negotiation/scenario，
//! 63 次 gamespec_build、62 次失败、单 turn 撞 64 步上限）。
//!
//! 用法：`SHANGYI_REPLAY=/tmp/failed_spec_args.json cargo test -p shangyi-core --test replay_real_failures -- --nocapture`

use shangyi_core::test_support::normalize_spec_for_replay;

#[test]
fn replay_real_failed_inputs() {
    let Ok(path) = std::env::var("SHANGYI_REPLAY") else {
        eprintln!("未设 SHANGYI_REPLAY，跳过（这是证据性测试，不是常规回归）");
        return;
    };
    let raw = std::fs::read_to_string(&path).expect("读样本文件");
    let args: Vec<String> = serde_json::from_str(&raw).expect("样本应为字符串数组");

    let mut ok = 0;
    let mut still_fail = Vec::new();
    for (i, a) in args.iter().enumerate() {
        // 样本存的是「arguments 字段的字符串形态」，即 JSON 编码后的参数
        let parsed: serde_json::Value = match serde_json::from_str(a) {
            Ok(v) => v,
            Err(e) => {
                still_fail.push(format!("[{i}] 样本本身无法解析：{e}"));
                continue;
            }
        };
        match normalize_spec_for_replay(&parsed) {
            Ok(_) => ok += 1,
            Err(e) => still_fail.push(format!("[{i}] {}", e.lines().next().unwrap_or(""))),
        }
    }
    println!("\n=== 回放结果：{} / {} 条现在可通过归一化 ===", ok, args.len());
    for f in still_fail.iter().take(15) {
        println!("  仍失败 {f}");
    }
    println!(
        "\n⚠️ 注意：归一化通过 ≠ 校验通过。`assumed` 参数仍会被铁律 2 拒绝——\
         但它们现在会拿到**指路的报错**（见 validation_message_tests）。"
    );
}
