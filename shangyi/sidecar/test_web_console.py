"""Regression checks for the Web consultation question contract. No engine calls."""
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
import web_console as web


class PlaybookProjectionTests(unittest.TestCase):
    def test_real_question_lists_keep_last_question(self):
        expected = {"pricing_response": ("q5", 5), "competitor_intel": ("q5", 5),
                    "negotiation": ("q6", 6), "market_entry": ("q6", 6)}
        for name, (last, count) in expected.items():
            with self.subTest(name=name):
                pb = web.parse_playbook(web.HERE.parent / "playbooks" / name / "playbook.yml")
                self.assertEqual(len(pb["intake"]), count)
                self.assertEqual(pb["intake"][-1]["id"], last)

    def test_final_question_survives_eof_and_next_section(self):
        source = 'meta:\n  id: example\nintake:\n  - id: final\n    text: "最后的问题"\n'
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "playbook.yml"
            for tail in ("", "models:\n  - repeated_game\n"):
                p.write_text(source + tail)
                self.assertEqual(web.parse_playbook(p)["intake"],
                                 [{"id": "final", "text": "最后的问题"}])

    def test_home_override_matches_cli_priority(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            override = home / "playbooks" / "pricing_response" / "playbook.yml"
            override.parent.mkdir(parents=True)
            override.write_text('meta:\n  id: pricing_response\n  name: 本机版本\n'
                                'intake:\n  - id: local\n    text: 本机问题\nmodels:\n  - example\n')
            with patch.object(web, "shangyi_home", return_value=home):
                pbs = {p["id"]: p for p in web.list_playbooks()}
            self.assertEqual(pbs["pricing_response"]["intake"][0]["text"], "本机问题")
            self.assertIn("negotiation", pbs)

    def test_exit_status_is_not_confused_with_success(self):
        class Proc:
            returncode = 7
            def poll(self):
                return self.returncode
        with tempfile.TemporaryDirectory() as tmp:
            entry = {"proc": Proc(), "case": None, "reaped": True,
                     "log_path": Path(tmp) / "absent", "run_id": "test",
                     "mode": "scenario", "action": None, "cmd": [], "started_at": 0}
            state = web.run_status(entry)
            self.assertFalse(state["alive"])
            self.assertEqual(state["exit_code"], 7)


class CostScenarioProjectionTests(unittest.TestCase):
    def test_case_retains_scenarios_and_updates_only_matching_successful_result(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            case = home / "cases" / "2026-10-02" / "case-20261002-costs"
            case.mkdir(parents=True)
            (case / "header.json").write_text('{}')
            outputs = [({}, 10, "ok"), ({"scenario": "A店"}, 100, "ok"),
                       ({"scenario": "B店"}, 200, "ok"), ({"scenario": "A店"}, 150, "ok"),
                       ({"scenario": "B店"}, 999, "failed")]
            events = []
            for index, (scenario, amount, status) in enumerate(outputs):
                events.extend([
                    {"event": {"type": "tool_call_started", "call_id": str(index), "name": "business_cost"}},
                    {"event": {"type": "tool_call_finished", "call_id": str(index), "outcome": {
                        "status": status, "output": json.dumps({**scenario, "listed_upfront_cash": amount})}}},
                ])
            (case / "case.jsonl").write_text(''.join(json.dumps(e) + '\n' for e in events))
            with patch.object(web, "shangyi_home", return_value=home):
                artifacts = web.project_case("2026-10-02", "case-20261002-costs")["artifacts"]
            self.assertEqual([c.get("scenario") for c in artifacts["business_costs"]], [None, "A店", "B店"])
            self.assertEqual([c["listed_upfront_cash"] for c in artifacts["business_costs"]], [10, 150, 200])
            self.assertEqual(artifacts["business_cost"]["scenario"], "A店")


class GameSpecProjectionTests(unittest.TestCase):
    @staticmethod
    def skeleton():
        return {"game_id": "original", "players": [
            {"id": "us", "name": "我方", "vn_role": "focal", "relation_to_game": "player"},
            {"id": "them", "name": "对方", "vn_role": "substitutor", "relation_to_game": "player"}],
            "strategies": {"us": ["进"], "them": ["挡"]},
            "payoffs": {"form": "normal", "values": {"cells": {"进,挡": {"us": 1, "them": 2}}}},
            "params": {}, "meta": {"unit": "万元"}}

    def project(self, calls):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            folder = home / "cases" / "2026-10-02" / "case-20261002-spec"
            folder.mkdir(parents=True)
            (folder / "header.json").write_text('{}')
            events = []
            for index, (arguments, status) in enumerate(calls):
                events.extend([
                    {"event": {"type": "tool_call_started", "call_id": str(index),
                               "name": "gamespec_build", "arguments": json.dumps(arguments)}},
                    {"event": {"type": "tool_call_finished", "call_id": str(index),
                               "outcome": {"status": status, "output": '{"validated":true}'}}}])
            (folder / "case.jsonl").write_text(''.join(json.dumps(e) + '\n' for e in events))
            with patch.object(web, "shangyi_home", return_value=home):
                return web.project_case("2026-10-02", "case-20261002-spec")

    def test_direct_and_nested_envelopes_produce_the_same_complete_matrix(self):
        spec = self.skeleton()
        forms = [spec, {"spec": spec}, {"arguments": json.dumps(spec)},
                 {"spec": {"arguments": json.dumps({"input": spec})}}, json.dumps(spec)]
        projected = [self.project([(args, "ok")])["artifacts"]["gamespec"] for args in forms]
        for actual in projected:
            self.assertEqual(actual, projected[0])
            self.assertEqual(len(actual["players"]), 2)
            self.assertEqual(actual["payoffs"]["values"]["cells"]["进,挡"]["us"], {"kind": "literal", "value": 1})
            self.assertFalse({"spec", "arguments", "input", "merge"} & actual.keys())

    def test_partial_merge_keeps_other_fields_and_failed_patch_cannot_change_them(self):
        patch = {"merge": True, "spec": {"params": {
            "cost": {"name": "成本", "value": 3, "unit": "万元",
                     "source": {"kind": "user_confirmed", "checkpoint": "cp2"}}},
            "payoffs": {"values": {"cells": {"进,挡": {"us": "cost"}}}}}}
        failed = {"merge": True, "params": {"cost": {"value": 999, "source": {"kind": "assumed", "note": "未知"}}},
                  "players": []}
        data = self.project([(self.skeleton(), "ok"), (patch, "ok"), (failed, "failed")])
        spec = data["artifacts"]["gamespec"]
        self.assertEqual(spec["game_id"], "original")
        self.assertEqual(len(spec["players"]), 2)
        self.assertEqual(spec["strategies"], {"us": ["进"], "them": ["挡"]})
        self.assertEqual(spec["params"]["cost"]["value"], {"kind": "literal", "value": 3})
        self.assertEqual(spec["payoffs"]["values"]["cells"]["进,挡"], {
            "us": {"kind": "ref", "value": "cost"}, "them": {"kind": "literal", "value": 2}})
        self.assertEqual(data["stats"]["tools"]["gamespec_build"], {"ok": 2, "failed": 1, "blocked": 0})

    def test_valid_merge_replaces_arrays_scalars_and_null_without_losing_untouched_fields(self):
        base = self.skeleton()
        base["external_actors"] = [{"id": "extra", "name": "外部参与者", "vn_role": "supplier", "relation_to_game": "external_actor"}]
        base["preference"] = {"risk_aversion": 0.8, "objective_weights": {}}
        players = self.skeleton()["players"]
        players[0]["name"] = "修改后的名字"
        patch = {"merge": True, "game_id": "revised", "players": players, "external_actors": [], "preference": None}
        spec = self.project([(base, "ok"), (patch, "ok")])["artifacts"]["gamespec"]
        self.assertEqual(spec["players"], players)
        self.assertEqual(spec["external_actors"], [])
        self.assertIsNone(spec["preference"])
        self.assertEqual(spec["game_id"], "revised")
        self.assertEqual(spec["meta"], {"unit": "万元"})

    def test_merge_flag_is_only_a_top_level_boolean_before_unwrapping(self):
        replacement = self.skeleton()
        replacement["game_id"] = "replacement"
        replacement["meta"] = {}
        replacement["merge"] = True
        for args in (replacement | {"merge": "true"}, {"spec": replacement}, json.dumps(replacement)):
            with self.subTest(arguments=args):
                spec = self.project([(self.skeleton(), "ok"), (args, "ok")])["artifacts"]["gamespec"]
                self.assertEqual(spec["game_id"], "replacement")
                self.assertEqual(spec["meta"], {}, "inner or string merge flags must not retain the earlier unit")
        outer = self.project([(self.skeleton(), "ok"), ({"merge": True, "spec": replacement}, "ok")])["artifacts"]["gamespec"]
        self.assertEqual(outer["meta"], {"unit": "万元"})

    def test_stringified_containers_are_normalized_before_merging(self):
        base = self.skeleton()
        base["dynamics"] = {"form": "repeated", "values": {"discount": 0.9, "horizon": 4}}
        patch = {"merge": True, "arguments": json.dumps({
            "payoffs": json.dumps({"values": {"cells": {"进,挡": {"us": 7}}}}),
            "params": "{}", "meta": json.dumps({"framework": ["A", "B"]}),
            "information": "complete", "dynamics": {"type": "repeated", "horizon": "indefinite"}})}
        spec = self.project([(base, "ok"), (patch, "ok")])["artifacts"]["gamespec"]
        self.assertEqual(spec["payoffs"]["form"], "normal")
        self.assertEqual(spec["payoffs"]["values"]["cells"]["进,挡"]["us"], {"kind": "literal", "value": 7})
        self.assertEqual(spec["dynamics"], {"form": "repeated", "values": {"discount": 0.9}})
        self.assertEqual(spec["information"], {"type": "complete"})
        self.assertEqual(spec["meta"], {"unit": "万元", "framework": "A、B"})

    def test_top_level_model_fields_take_precedence_and_unreadable_success_is_not_stale_state(self):
        spec = self.skeleton()
        spec["spec"] = self.skeleton() | {"game_id": "wrong-envelope"}
        projected = self.project([(spec, "ok")])["artifacts"]["gamespec"]
        self.assertEqual(projected["game_id"], "original")
        artifacts = self.project([(spec, "ok"), ({"arguments": "not JSON"}, "ok")])["artifacts"]
        self.assertNotIn("gamespec", artifacts)
        self.assertIn("gamespec_projection_error", artifacts)


class ScopedTaskProjectionTests(unittest.TestCase):
    @staticmethod
    def result(status="partial", summary="已列费用；押金仍未取得。"):
        return {"scope": "cost", "status": status, "summary": summary,
                "missing": ["押金金额未取得"] if status == "partial" else [],
                "source_urls": ["https://example.com/source"], "retrieval_attempts": 2,
                "cost_scenarios": [{"scenario": "已知费用", "listed_upfront_cash": 110000}]}

    def project(self, events, conversations=()):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            folder = home / "cases" / "2026-10-02" / "case-20261002-scoped"
            folder.mkdir(parents=True)
            (folder / "header.json").write_text('{"mode":"scenario"}')
            (folder / "case.jsonl").write_text(''.join(json.dumps({"event": e}) + '\n' for e in events))
            for state in conversations:
                path = home / "web-conversations" / state["id"] / "state.json"
                path.parent.mkdir(parents=True)
                path.write_text(json.dumps(state))
            with patch.object(web, "shangyi_home", return_value=home):
                return web.project_case("2026-10-02", "case-20261002-scoped")

    def test_intake_completion_keeps_the_actual_summary_and_all_result_evidence(self):
        result = self.result()
        case = self.project([{"type": "phase_entered", "to": "intake", "at": "2026-10-02T11:00:00Z"},
                             {"type": "scoped_task_completed", "result": result, "at": "2026-10-02T11:01:00Z"}])
        self.assertEqual(case["stats"]["phases"], ["intake"])
        artifacts = case["artifacts"]
        expected = {**result, "at": "2026-10-02T11:01:00Z"}
        self.assertEqual(artifacts["task_results"], [expected])
        self.assertEqual(artifacts["latest_task"], expected)
        self.assertIsNot(artifacts["task_results"][0], artifacts["latest_task"])
        exported = json.loads(json.dumps(case))
        self.assertEqual(exported["artifacts"]["task_results"][0]["cost_scenarios"], result["cost_scenarios"])

    def test_followup_invalidates_current_completion_without_rewriting_prior_results(self):
        result = self.result("complete", "本次费用清单已整理。")
        events = [{"type": "scoped_task_completed", "result": result, "at": "2026-10-02T11:01:00Z"},
                  {"type": "followup_task_started", "at": "2026-10-02T11:02:00Z"}]
        artifacts = self.project(events)["artifacts"]
        self.assertEqual(artifacts["latest_task"], {"status": "processing", "at": "2026-10-02T11:02:00Z"})
        self.assertEqual(artifacts["task_results"][0]["status"], "complete")
        self.assertEqual(artifacts["task_results"][0]["summary"], result["summary"])
        self.assertNotIn("summary", artifacts["latest_task"])
        later = self.result("partial", "追加后的清单已整理，但税费仍未知。")
        events.append({"type": "scoped_task_completed", "result": later, "at": "2026-10-02T11:03:00Z"})
        artifacts = self.project(events)["artifacts"]
        self.assertEqual(len(artifacts["task_results"]), 2)
        self.assertEqual(artifacts["task_results"][0]["summary"], result["summary"])
        self.assertEqual(artifacts["latest_task"], {**later, "at": "2026-10-02T11:03:00Z"})

    def test_other_events_cannot_invent_a_scoped_completion(self):
        artifacts = self.project([{"type": "phase_entered", "to": "intake"},
                                  {"type": "scoped_task_completed", "result": None},
                                  {"type": "tool_call_finished", "call_id": "failed", "outcome": {"status": "failed"}}])["artifacts"]
        self.assertEqual(artifacts["task_results"], [])
        self.assertIsNone(artifacts["latest_task"])

    def test_original_question_comes_from_one_exact_real_state_file_not_an_assumed_archive(self):
        initial = "只整理搬店费用。\n租金每月9000元，押金只列资金占用；不要预测利润。"
        state = {"id": "chat-" + "1" * 24, "initial": initial,
                 "case": "2026-10-02/case-20261002-scoped", "topic": "market_entry", "mode": "scenario"}
        wrong_date = {**state, "id": "chat-" + "2" * 24, "case": "2026-10-01/case-20261002-scoped", "initial": "不是同一日期的记录"}
        events = [{"type": "scoped_task_completed", "result": self.result(), "at": "2026-10-02T11:01:00Z"}]
        case = self.project(events, [state, wrong_date])
        self.assertEqual(case["question"], initial)
        self.assertEqual(case["question_source"], "web_conversation_state")
        self.assertEqual(case["context"], {"source": "web_conversation_state", "chat_id": state["id"],
                                            "initial": initial, "case": state["case"]})
        self.assertEqual(json.loads(json.dumps(case))["context"]["initial"], initial)
        ambiguous = self.project(events, [state, {**state, "id": wrong_date["id"], "initial": "另一份匹配问题"}])
        self.assertIsNone(ambiguous["question"])
        self.assertIsNone(ambiguous["question_source"])
        self.assertIsNone(ambiguous["context"])


if __name__ == "__main__":
    unittest.main()
