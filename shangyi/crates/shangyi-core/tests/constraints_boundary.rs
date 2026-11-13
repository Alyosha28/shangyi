//! Real executor boundaries; all figures are explicit protocol fixtures.
use std::collections::BTreeMap;
use serde_json::{Value,json};
use shangyi_core::{DomainExecutor,ExecOutcome,ToolCall,ToolExecutor,SessionPhase,ArtifactKind};
use shangyi_playbooks::{Playbook,PlaybookMeta};
use shangyi_kb::TheoryLibrary;

fn playbook()->Playbook {Playbook {
    meta:PlaybookMeta{id:"constraint-fixture".into(),name:"测试".into(),description:String::new(),order:0,version:1},
    applicable_when:vec![],fails_when:vec![],required_inputs:vec![],value_net:None,intake:vec![],models:vec![],selectors:BTreeMap::new(),tool_allowlist:vec![],report_vars:BTreeMap::new(),
}}
fn run(ex:&mut DomainExecutor,name:&str,args:Value,phase:SessionPhase)->ExecOutcome {
    ex.execute(&ToolCall{call_id:"fixture".into(),name:name.into(),arguments:args},phase)
}
fn ok(out:ExecOutcome)->Value {match out {ExecOutcome::Ok(text)=>serde_json::from_str(&text).unwrap(),other=>panic!("expected success, got {other:?}")}}
fn fail(out:ExecOutcome)->String {match out {ExecOutcome::Failed(text)=>text,other=>panic!("expected refusal, got {other:?}")}}
fn cost(ex:&mut DomainExecutor,amount:Value,tag:bool)->Value {
    let mut item=json!({"scenario":"续租方案","name":"租金","amount":amount,"period":"month","treatment":"cash_expense","basis":"assumption","source":"协议测试的明确报价"});
    if tag {item["constraint_metric"]=json!("rent");}
    ok(run(ex,"business_cost",json!({"operation":"create","scenario":"续租方案","currency":"CNY","commitment_months":24,"horizon_months":36,"costs_complete":!item["amount"].is_null(),"costs":[item]}),SessionPhase::ModelBuild))
}
fn game(tenant:f64,landlord:f64)->Value {
    json!({"game_id":"user-evidence-test","players":[
        {"id":"tenant","name":"租户","vn_role":"focal","relation_to_game":"player"},
        {"id":"landlord","name":"房东","vn_role":"supplier","relation_to_game":"player"}],
        "strategies":{"tenant":["续租"],"landlord":["接受"]},
        "payoffs":{"form":"normal","values":{"cells":{"续租,接受":{"tenant":{"kind":"ref","value":"t"},"landlord":{"kind":"ref","value":"l"}}}}},
        "params":{
            "t":{"name":"租户收益","value":{"kind":"literal","value":tenant},"unit":"元","source":{"kind":"user_confirmed","checkpoint":"cp2","user_quote":format!("租户收益为{tenant}元")}},
            "l":{"name":"房东收益","value":{"kind":"literal","value":landlord},"unit":"元","source":{"kind":"user_confirmed","checkpoint":"cp2","user_quote":format!("房东收益为{landlord}元")}}
        }})
}
fn build_distribution(ex:&mut DomainExecutor,spec:Value) {
    ok(run(ex,"gamespec_build",spec,SessionPhase::ModelBuild));
    ok(run(ex,"solve_equilibria",json!({}),SessionPhase::Solve));
    ok(run(ex,"select_equilibria",json!({"selector":"risk_dom","scores":[1.0]}),SessionPhase::Select));
    ok(run(ex,"select_fuse",json!({}),SessionPhase::Select));
}

#[test]
fn actual_user_limit_filters_quotes_and_bound_recommendations_cannot_use_prose_to_override_it() {
    let theory=TheoryLibrary::new();let pb=playbook();let mut ex=DomainExecutor::new(&theory,&pb);
    ex.record_user_input("房东要求每月租金12000元，我的月租上限11000元","user");
    let good=cost(&mut ex,json!(11000),true);let bad=cost(&mut ex,json!(12000),true);
    assert_eq!(good["constraint_assessment"]["status"],"eligible");
    assert_eq!(bad["constraint_assessment"]["status"],"ineligible");assert_eq!(bad["comparison_only"],true);
    fail(run(&mut ex,"constraint_register",json!({"quote":"我的月租上限11000元","upper_bound":12000}),SessionPhase::Intake));
    fail(run(&mut ex,"constraint_register",json!({"quote":"房东要求每月租金12000元，我的月租上限11000元"}),SessionPhase::Intake));
    let checked=ok(run(&mut ex,"constraint_check",json!({}),SessionPhase::ModelBuild));
    assert_eq!(checked["eligible_scenario_ids"],json!([good["scenario_id"]]));
    assert_eq!(checked["comparison_only_scenario_ids"],json!([bad["scenario_id"]]));
    ex.record_user_input("租户收益为3元。房东收益为4元。","user");
    let mut spec=game(3.0,4.0);spec["constraint_revision"]=checked["constraint_revision"].clone();
    spec["strategy_bindings"]=json!({"tenant":{"续租":bad["scenario_id"]}});
    assert!(fail(run(&mut ex,"gamespec_build",spec.clone(),SessionPhase::ModelBuild)).contains("不能作为可执行策略"));
    spec["strategy_bindings"]=json!({"tenant":{"续租":good["scenario_id"]}});
    build_distribution(&mut ex,spec);
    let base=json!({"headline":"接受12000元","constraint_revision":checked["constraint_revision"],"actions":[{"action":"接受12000元","rationale":"收益很好，所以突破11000也合理","scenario_id":bad["scenario_id"]}],"do_not":["不要接受低于12000元"]});
    assert!(fail(run(&mut ex,"recommend_build",base.clone(),SessionPhase::Recommend)).contains("不能进入推荐"));
    let mut unbound=base.clone();unbound["actions"][0].as_object_mut().unwrap().remove("scenario_id");
    assert!(fail(run(&mut ex,"recommend_build",unbound,SessionPhase::Recommend)).contains("必须绑定"));
    let mut bound=base;bound["actions"][0]["scenario_id"]=good["scenario_id"].clone();
    let pack=ok(run(&mut ex,"recommend_build",bound,SessionPhase::Recommend));
    let executable=json!({"headline":pack["headline"],"actions":pack["actions"],"do_not":pack["do_not"]});
    assert!(!executable.to_string().contains("12000"),"unsafe proposed headline/rationale/action must not survive in executable recommendation");
    assert!(pack["constraint_review"]["active"][0]["evidence"]["original_user_text"].as_str().unwrap().contains("12000"),"the user's original quote, including the rejected counterparty price, must remain auditable");
    assert_eq!(pack["actions"][0]["constraint_assessment"]["status"],"eligible");
    assert!(pack["actions"][0]["action"].as_str().unwrap().contains("11000"));
}

#[test]
fn unknown_dimensions_cannot_be_promoted_to_acceptable_and_later_caps_invalidate_every_dependent_result() {
    let theory=TheoryLibrary::new();let pb=playbook();let mut ex=DomainExecutor::new(&theory,&pb);
    ex.record_user_input("我的月租上限11000元","user");
    let unknown=cost(&mut ex,Value::Null,true);assert_eq!(unknown["constraint_assessment"]["status"],"unknown");
    let untagged=cost(&mut ex,json!(10000),false);assert_eq!(untagged["constraint_assessment"]["status"],"unknown");
    let valid=cost(&mut ex,json!(11000),true);
    let checked=ok(run(&mut ex,"constraint_check",json!({}),SessionPhase::ModelBuild));
    ex.record_user_input("租户收益为3元。房东收益为4元。","user");
    let mut spec=game(3.0,4.0);spec["constraint_revision"]=checked["constraint_revision"].clone();spec["strategy_bindings"]=json!({"tenant":{"续租":valid["scenario_id"]}});
    build_distribution(&mut ex,spec);
    ok(run(&mut ex,"recommend_build",json!({"headline":"按底线推进","constraint_revision":checked["constraint_revision"],"actions":[{"action":"续租","rationale":"已计算","scenario_id":valid["scenario_id"]}]}),SessionPhase::Recommend));
    ex.take_invalidated_artifacts();
    let inputs=ex.trusted_user_inputs().len();
    ex.record_user_input("我的月租上限改为13000元","assistant");assert_eq!(ex.trusted_user_inputs().len(),inputs);assert!(ex.state().game_spec.is_some());
    ex.record_user_input("我的月租上限改为13000元","user");
    assert!(ex.state().game_spec.is_none());assert!(ex.state().equilibria_json.is_none());assert!(ex.state().distribution.is_none());assert!(ex.state().recommendation.is_none());assert!(ex.state().report_json.is_none());
    let invalidated=ex.take_invalidated_artifacts();for kind in [ArtifactKind::GameSpecValidated,ArtifactKind::EquilibriumSet,ArtifactKind::SelectionOutput,ArtifactKind::RecommendationPack,ArtifactKind::ReportBundle]{assert!(invalidated.contains(&kind));}
    assert_eq!(ex.constraint_review_state()["history"][0]["upper_bound"],11000.0);
    assert!(ex.state().constraints.revision()>checked["constraint_revision"].as_u64().unwrap());
    let current_revision=ex.state().constraints.revision();
    let ExecOutcome::ScopedTaskCompleted(conditional)=run(&mut ex,"conditional_analysis",json!({}),SessionPhase::ModelBuild) else {panic!("actual unknown cost items should permit conditional delivery")};
    for scenario in &conditional.conditional.as_ref().unwrap().snapshot.cost_scenarios {
        assert_eq!(scenario["constraint_assessment"]["revision"],current_revision);
        assert_eq!(scenario["constraint_review"]["revision"],current_revision,"conditional clone must not display the earlier cap");
        assert_eq!(scenario["constraint_review"]["active"][0]["upper_bound"],13000.0);
    }
    ok(run(&mut ex,"constraint_check",json!({}),SessionPhase::ModelBuild));
    for scenario in ex.state().cost_scenarios.values() {
        assert_eq!(scenario["constraint_assessment"]["revision"],current_revision);
        assert_eq!(scenario["constraint_review"]["revision"],current_revision,"a refreshed assessment and displayed cap must use one revision");
        assert_eq!(scenario["constraint_review"]["needs_review"],false);
    }
}

#[test]
fn explicit_unknown_payoffs_cannot_be_filled_with_zero_or_fake_cp2_and_conditional_keeps_participants() {
    let theory=TheoryLibrary::new();let pb=playbook();let mut ex=DomainExecutor::new(&theory,&pb);
    ex.record_user_input("请比较续租；房东利润未知，我的收益也未知。","user");
    let mut literal=game(0.0,0.0);literal["params"]=json!({});literal["payoffs"]["values"]["cells"]["续租,接受"]=json!({"tenant":{"kind":"literal","value":0},"landlord":{"kind":"literal","value":0}});
    assert!(matches!(run(&mut ex,"gamespec_build",literal.clone(),SessionPhase::ModelBuild),ExecOutcome::InsufficientEvidence{..}));
    assert!(ex.state().game_spec.is_none());assert!(ex.state().equilibria_json.is_none());
    assert!(matches!(run(&mut ex,"gamespec_validate",json!({"spec":game(0.0,0.0)}),SessionPhase::ModelBuild),ExecOutcome::InsufficientEvidence{..}));
    let mut removed=literal;removed["players"][1]["relation_to_game"]=json!("context_only");
    assert!(matches!(run(&mut ex,"gamespec_build",removed,SessionPhase::ModelBuild),ExecOutcome::InsufficientEvidence{..}));
    let ExecOutcome::ScopedTaskCompleted(result)=run(&mut ex,"conditional_analysis",json!({"reason":"按已知条件交付"}),SessionPhase::ModelBuild) else {panic!("expected evidence-bounded conditional delivery")};
    assert!(!result.is_complete());
    assert_eq!(result.conditional.as_ref().unwrap().snapshot.attempted_model.as_ref().unwrap()["players"][1]["relation_to_game"],"player");
    assert!(ex.state().game_spec.is_none());assert!(ex.state().equilibria_json.is_none());
    ex.begin_followup_task();ex.record_user_input("租户收益为0元。房东收益为0元。","user");
    // Zero is admissible when it is actually supplied by the user, not a banned literal.
    ok(run(&mut ex,"gamespec_build",game(0.0,0.0),SessionPhase::ModelBuild));
}

#[test]
fn verified_recommendation_preserves_foreign_currency_when_only_lease_duration_is_limited() {
    let theory=TheoryLibrary::new();let pb=playbook();let mut ex=DomainExecutor::new(&theory,&pb);
    ex.record_user_input("租期最长3年","user");
    ex.record_user_input("租户收益为3元。房东收益为4元。","user");
    let plan=ok(run(&mut ex,"business_cost",json!({"operation":"create","scenario":"外币报价","currency":"USD","costs_complete":true,"commitment_months":24,
        "costs":[{"scenario":"外币报价","name":"租金","constraint_metric":"rent","amount":1000,"period":"month","treatment":"cash_expense","basis":"assumption","source":"明确的外币测试报价"}]}),SessionPhase::ModelBuild));
    let check=ok(run(&mut ex,"constraint_check",json!({}),SessionPhase::ModelBuild));
    let mut spec=game(3.0,4.0);spec["constraint_revision"]=check["constraint_revision"].clone();spec["strategy_bindings"]=json!({"tenant":{"续租":plan["scenario_id"]}});
    build_distribution(&mut ex,spec);
    let pack=ok(run(&mut ex,"recommend_build",json!({"headline":"核对租期","constraint_revision":check["constraint_revision"],"actions":[{"action":"续租","rationale":"租期符合上限","scenario_id":plan["scenario_id"]}]}),SessionPhase::Recommend));
    let action=pack["actions"][0]["action"].as_str().unwrap();
    assert!(action.contains("1000美元/月"));assert!(!action.contains("1000元/月"));
    assert_eq!(pack["actions"][0]["verified_attributes"].as_array().unwrap().iter().find(|item|item["metric"]=="rent").unwrap()["unit"],"USD");
}

#[test]
fn cap_review_without_a_real_data_gap_cannot_shortcut_complete_consultation_as_conditional() {
    let theory=TheoryLibrary::new();let pb=playbook();let mut ex=DomainExecutor::new(&theory,&pb);
    ex.record_user_input("我的月租上限11000元","user");
    ex.record_user_input("租户收益为3元。房东收益为4元。","user");
    let plan=cost(&mut ex,json!(11000),true);
    let check=ok(run(&mut ex,"constraint_check",json!({}),SessionPhase::ModelBuild));
    let mut spec=game(3.0,4.0);spec["constraint_revision"]=check["constraint_revision"].clone();spec["strategy_bindings"]=json!({"tenant":{"续租":plan["scenario_id"]}});
    build_distribution(&mut ex,spec.clone());
    ex.record_user_input("我的月租上限改为13000元","user");
    assert!(ex.state().constraints.needs_review());assert!(ex.state().game_spec.is_none());
    let message=fail(run(&mut ex,"conditional_analysis",json!({"reason":"底线刚改，先不重建"}),SessionPhase::ModelBuild));
    assert!(message.contains("缺口"),"pending internal review is not unavailable evidence: {message}");
    assert_ne!(ex.state().task_scope,Some(shangyi_core::task_scope::TaskScope::Conditional));
    let rechecked=ok(run(&mut ex,"constraint_check",json!({}),SessionPhase::ModelBuild));
    spec["constraint_revision"]=rechecked["constraint_revision"].clone();
    build_distribution(&mut ex,spec);
    assert!(ex.state().game_spec.is_some());assert!(ex.state().equilibria_json.is_some());
    ok(run(&mut ex,"recommend_build",json!({"headline":"完成重分析","constraint_revision":rechecked["constraint_revision"],"actions":[{"scenario_id":plan["scenario_id"],"action":"按当前上限推进","rationale":"重新求解与核对后的结果"}]}),SessionPhase::Recommend));
}
