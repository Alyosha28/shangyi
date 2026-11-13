//! 序列化契约的**结构性防线**。
//!
//! # 为什么需要这个文件（同一个坑踩了三次）
//!
//! `GameSpec` 顶层有一个 `params` 字段（参数表）。若某个**邻接标记枚举**
//! 的 content 键也叫 `params`，serde 的 `missing field \`params\`` 就同时
//! 表示多件事：
//!
//! | 报错 | 真实该做什么 | 注释里说清楚了吗 |
//! |---|---|---|
//! | 顶层缺 `params` | 补参数表 | ✅ |
//! | `payoffs` 缺 content | 把 `cells` 包进 `values` | ✅（已改名） |
//! | `dynamics` 缺 content | 补 `values: {discount: …}` | ❌ **漏改** |
//!
//! 前两处按规则改了，第三处**漏了**——因为规则只写在注释里。
//! 实测代价：一次真实咨询里模型为 `dynamics` 的形状连续失败 **7 次**
//! （依次试 `{"repeated":true}` → `{"form":"repeated"}` →
//! `{"type":"repeated"}` → 两者都给），每次拿到的都是
//! `missing field params` ⇒ 它一直去补那个**本来就在**的顶层字段。
//!
//! 故本文件的测试**不靠人记规则**：直接序列化实际值，检查键名。
//! 新增一个邻接标记枚举时若撞名，这里立刻失败。

use std::collections::BTreeMap;

use shangyi_gamespec::*;

fn minimal_spec(dynamics: Option<Dynamics>) -> GameSpec {
    let mut strategies = BTreeMap::new();
    strategies.insert("a".to_string(), vec!["x".to_string()]);
    GameSpec {
        game_id: "probe".into(),
        players: vec![Participant {
            id: "a".into(),
            name: "甲".into(),
            vn_role: ValueNetRole::Focal,
            relation_to_game: RelationToGame::Player,
            profile_ref: None,
        }],
        strategies,
        payoffs: PayoffForm::Normal {
            cells: BTreeMap::new(),
        },
        params: BTreeMap::new(),
        information: None,
        dynamics,
        external_actors: vec![],
        preference: None,
        meta: BTreeMap::new(),
    }
}

/// **邻接标记枚举的 content 键不得与顶层字段撞名。**
///
/// 检查方式：把含各类枚举的 `GameSpec` 序列化成 JSON，看嵌套对象里
/// 是否出现了 `params` 键——顶层那个 `params` 不算，只看嵌套的。
///
/// 这条断言能抓住的正是"加了新枚举、忘了改名"这一类漏改。
#[test]
fn no_tagged_enum_collides_with_top_level_params() {
    let spec = minimal_spec(Some(Dynamics::Repeated {
        discount: 0.9,
        horizon: Some(5),
    }));
    let v = serde_json::to_value(&spec).expect("可序列化");

    // dynamics 内部不得出现 `params`
    let dyn_v = v.get("dynamics").expect("dynamics 应在");
    assert!(
        dyn_v.get("params").is_none(),
        "`dynamics` 内出现了 `params` 键 ⇒ 它与顶层 `params` 撞名，\
         会使 `missing field params` 有多重含义（实测让模型连续失败 7 次）。\
         实得：{dyn_v}"
    );
    assert!(
        dyn_v.get("values").is_some(),
        "`dynamics` 应使用 `values` 作 content 键（与 PayoffForm/Dist 一致）：{dyn_v}"
    );

    // payoffs 内部同理
    let pay = v.get("payoffs").expect("payoffs 应在");
    assert!(
        pay.get("params").is_none(),
        "`payoffs` 内出现了 `params` 键（应为 `values`）：{pay}"
    );
}

/// 参数分布（`Dist`）同样不得用 `params` 作 content 键。
#[test]
fn dist_content_key_is_not_params() {
    let d = Dist::Normal {
        mu: 1.0,
        sigma: 0.2,
    };
    let v = serde_json::to_value(&d).expect("可序列化");
    assert!(v.get("params").is_none(), "`Dist` 撞名：{v}");
    assert!(v.get("values").is_some(), "`Dist` 应使用 `values`：{v}");
}

/// **往返必须无损**——序列化规则改了之后，旧形状的数据若还能解析，
/// 说明改名没生效（或 serde 走了宽容路径）。
///
/// 这里断言的是：新形状可往返，且**旧的 `params` 形状被拒**。
/// 后者是有意的——静默接受旧形状会让"改名"变成"两套形状都活"，
/// 而那种模糊正是本文件要消灭的东西。
#[test]
fn dynamics_round_trips_and_rejects_the_old_shape() {
    let d = Dynamics::Repeated {
        discount: 0.9,
        horizon: Some(5),
    };
    let v = serde_json::to_value(&d).unwrap();
    let back: Dynamics = serde_json::from_value(v).expect("新形状应可往返");
    assert_eq!(d, back, "往返必须无损");

    // 旧形状（content = "params"）
    let old = serde_json::json!({"form": "repeated", "params": {"discount": 0.9}});
    assert!(
        serde_json::from_value::<Dynamics>(old).is_err(),
        "旧形状应被拒——若它被接受，说明改名没生效，两种形状会并存"
    );

    // `OneShot` 是无内容的变体：不带 content 才是对的
    let one = serde_json::json!({"form": "one_shot"});
    let one_back: Dynamics = serde_json::from_value(one).expect("OneShot 应可解析");
    assert_eq!(one_back, Dynamics::OneShot);
}

/// `Expr` 必须保持邻接标记——**这条不是风格偏好**，是
/// `arbitrary_precision` 下 internally/untagged 枚举在数字上会失败
/// （见 crate 文档的序列化规则）。
///
/// 若有人把它改成 internally tagged，本测试会失败——且**在单独测本 crate
/// 时就会失败**：该 feature 由本 crate 的 `[dev-dependencies]` 显式启用，
/// 故 `cargo test -p shangyi-gamespec` 与全量构建都跑在同一环境下。
/// （R1 前它靠上游 vendor crate 的全图 feature 合并生效，`-p` 下也能命中。）
#[test]
fn expr_stays_adjacently_tagged() {
    let e = Expr::Literal(3.0);
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v.get("kind").and_then(|k| k.as_str()), Some("literal"));
    assert_eq!(v.get("value").and_then(|x| x.as_f64()), Some(3.0));

    let r = Expr::Ref("margin".into());
    let rv = serde_json::to_value(&r).unwrap();
    assert_eq!(rv.get("kind").and_then(|k| k.as_str()), Some("ref"));
    assert_eq!(rv.get("value").and_then(|x| x.as_str()), Some("margin"));
}
