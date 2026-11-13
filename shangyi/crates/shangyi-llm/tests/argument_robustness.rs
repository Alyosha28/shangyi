//! `arguments` 的**健壮性**：非法内层 JSON 不得打死整条响应。
//!
//! 这是对一处真实缺陷的回归测试（2026-09-18 端到端运行时暴露）：
//! 输出被上限截断时 `arguments` 停在半句，而当时的实现让**整条响应**
//! 反序列化失败，错误信息写成「响应解析失败」——**真因是参数被截断**。
//!
//! 正确行为：坏参数原样保留为字符串，流到工具那里报「参数非法」。

use shangyi_llm::LlmResponse;

#[test]
fn malformed_arguments_string_does_not_kill_the_whole_response() {
    // 模拟：响应本身合法，但 function_call 的 arguments 是**半截 JSON**
    // （实测遇到的形态：输出被上限截断，arguments 停在半句）
    let text = r#"{
      "id": "r", "status": "completed",
      "output": [{
        "type": "function_call",
        "call_id": "c1",
        "name": "checkpoint",
        "arguments": "{\"items\": [{\"content\": \"对手是三家\""
      }]
    }"#;

    match serde_json::from_str::<LlmResponse>(text) {
        Ok(r) => {
            // 期望行为：响应解析成功，arguments 原样保留（字符串），
            // 由工具报告「参数非法」——那才是可操作的诊断
            let calls = r.function_calls();
            assert_eq!(calls.len(), 1);
            println!("  ✓ 响应解析成功，arguments = {:?}", calls[0].2);
        }
        Err(e) => panic!(
            "✗ 响应整体解析失败：{e}\n\
             这说明 `deserialize_args_from_either` 对非法内层 JSON **硬失败**，\n\
             而后果是整个 turn 被打死、错误信息指向「响应解析」——\n\
             一个指向错误方向的诊断（真因是参数被截断，该由工具报「参数非法」）"
        ),
    }
}
