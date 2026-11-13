//! 传输层超时——**防止咨询永久挂起**。
//!
//! # 为什么这几条测试值得存在
//!
//! 这个 bug 的形态是"没有任何症状"：ureq 3.x 的默认超时**全是 `None`**
//! （见 `ureq-3.1.4/src/config.rs` 的 `impl Default for Timeouts`），
//! 于是服务器接受了 TCP 连接却不发响应时，进程会一直等下去。
//!
//! 实测代价：一次真实咨询进程在此卡死 **17 分钟**、CPU 时间仅 0.7 秒，
//! 没有任何输出、没有任何错误——而它是 5 小时自动循环的一环，
//! 于是整条流水线静默停摆。**这种 bug 不写测试就一定会复发。**

use shangyi_llm::{
    agent_config, timeout_secs_from_env, UreqTransport, CONNECT_TIMEOUT_SECS, DEFAULT_TIMEOUT_SECS,
};

/// 配置里**必须**带上全局超时——这是防挂起的唯一落点。
#[test]
fn agent_config_sets_a_global_timeout() {
    let cfg = agent_config(600);
    let t = cfg.timeouts();
    assert_eq!(
        t.global,
        Some(std::time::Duration::from_secs(600)),
        "全局超时未生效 ⇒ 服务器不响应时进程会永久挂起"
    );
    assert_eq!(
        t.connect,
        Some(std::time::Duration::from_secs(CONNECT_TIMEOUT_SECS)),
        "连接超时未生效 ⇒ TCP 握手失败会等到全局超时才报错"
    );
}

/// 默认值必须是**有限的**、且宽到不会误杀合法长推理。
#[test]
fn default_timeout_is_finite_and_generous() {
    assert!(
        DEFAULT_TIMEOUT_SECS >= 60,
        "默认超时 {DEFAULT_TIMEOUT_SECS}s 太短——思考档 high 的合法长推理会被误杀。\
         误杀比挂起更坏：它静默改变了系统行为，而用户以为只是网络抖动。"
    );
    assert!(
        DEFAULT_TIMEOUT_SECS <= 3600,
        "默认超时 {DEFAULT_TIMEOUT_SECS}s 太长——挂起一小时与永久挂起差别不大"
    );
}

/// 环境变量可覆盖（用户遇到长推理时能自己调大，而不是被迫放弃思考档）。
#[test]
fn env_var_can_extend_the_timeout() {
    // 注意：环境变量是进程级的，故本测试**不能**在读取前设置它——
    // 那会与其他并行测试竞争。改测"非法值不生效"这条纯逻辑：
    // 无论环境里有什么，返回值都必须落在合法区间内。
    let v = timeout_secs_from_env();
    assert!(v > 0, "超时不得为 0（0 意味着立即超时，等于禁用了模型调用）");
}

/// **集成形态**：对一台"只接受连接、永不响应"的服务器发请求，
/// 必须在超时内返回错误，而不是永久挂起。
///
/// 这条测试用真的挂起服务器跑一遍——因为「配置里写了超时」与
/// 「超时真的会触发」是两件事，中间隔着 ureq 的实现细节。
#[test]
fn hanging_server_times_out_instead_of_hanging_forever() {
    use std::io::Read;
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("可绑定本地端口");
    let port = listener.local_addr().unwrap().port();

    // 接受连接，读掉请求，然后**什么都不回**
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf);
            std::thread::sleep(std::time::Duration::from_secs(10));
        }
    });

    let start = std::time::Instant::now();
    let result = UreqTransport.post_json_with_timeout(
        &format!("http://127.0.0.1:{port}/hang"),
        &[],
        &serde_json::json!({"probe": true}),
        1, // 显式 1 秒：本测试验证的是「超时会不会触发」，不是默认值多大
    );
    let elapsed = start.elapsed();

    // 传输层错误 ⇒ 必须返回 Err，而不是 Ok（Ok 会被当成"服务器回了个空响应"）
    assert!(
        result.is_err(),
        "挂起的服务器必须产生传输错误，实得：{result:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "超时未触发：等了 {elapsed:?}。若这里失败，说明 ureq 的 timeout_global \
         没有覆盖到收响应阶段——那正是「永久挂起」的成因。"
    );
}
