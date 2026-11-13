#!/usr/bin/env python3
"""决策帧投影的单元测试：三态规则必须守住——不推测、缺项必须显式。"""

import unittest

from frame_projection import decision_frame


def spec(players, strategies):
    return {"gamespec": {"spec": {"players": players, "strategies": strategies}}}


class FrameBasics(unittest.TestCase):
    def test_empty_artifacts_leave_slots_empty_rather_than_guessed(self):
        frame = decision_frame(question="在上海南京路32号开店值不值\n第二行不算", topic="market_entry", mode="scenario")
        self.assertEqual(frame["question"], "在上海南京路32号开店值不值")
        self.assertEqual(frame["type"], "新生意")
        self.assertEqual(frame["mode"], "看可能")
        self.assertIsNone(frame["move"]["value"], "没给动作就不能编一个")
        self.assertEqual(frame["players"], [])
        self.assertEqual(frame["facts"], [])
        self.assertEqual(frame["assumptions"], [])
        self.assertEqual(frame["gaps"], [])
        self.assertIsNone(frame["result"])

    def test_action_is_marked_as_user_given(self):
        frame = decision_frame(action="跟着降价10%")
        self.assertEqual(frame["move"]["value"], "跟着降价10%")
        self.assertEqual(frame["move"]["via"], "你指定的")


class FramePlayers(unittest.TestCase):
    def test_players_and_moves_come_from_the_built_game(self):
        frame = decision_frame(artifacts=spec(
            [{"id": "me", "name": "我方", "vn_role": "focal", "relation_to_game": "player"},
             {"id": "mall", "name": "商场物业", "vn_role": "substitutor", "relation_to_game": "player"}],
            {"me": ["坚持底线", "让价"], "mall": ["强硬", "让步"]}))
        self.assertEqual([p["name"] for p in frame["players"]], ["我方", "商场物业"])
        self.assertEqual(frame["players"][0]["side"], "我方")
        self.assertEqual(frame["players"][1]["side"], "对方")
        self.assertEqual(frame["players"][1]["moves"], ["强硬", "让步"])

    def test_non_player_relations_are_not_listed_as_game_sides(self):
        frame = decision_frame(artifacts=spec(
            [{"id": "me", "name": "我方", "relation_to_game": "player"},
             {"id": "env", "name": "监管", "relation_to_game": "environment"}], {"me": ["开"]}))
        self.assertEqual([p["name"] for p in frame["players"]], ["我方"])


class FrameEvidence(unittest.TestCase):
    def test_quoted_field_is_verified_and_carries_its_source(self):
        frame = decision_frame(artifacts={"research": {"records": [
            {"url": "https://example.com/listing", "fields": {"rent": {"value": "8万/月", "quote": "月租金80000元"}}}]}})
        self.assertEqual(frame["facts"], [{"label": "rent", "value": "8万/月", "source": "https://example.com/listing"}])
        self.assertEqual(frame["assumptions"], [])

    def test_field_without_quote_is_input_or_assumption_not_verified(self):
        frame = decision_frame(artifacts={"research": {"records": [
            {"url": "https://example.com/listing", "fields": {"area": {"value": "40㎡"}}}]}})
        self.assertEqual(frame["facts"], [], "没有原文摘录就不算已核实")
        self.assertEqual(frame["assumptions"], [{"label": "area", "value": "40㎡", "via": "资料整理"}])

    def test_missing_field_becomes_an_explicit_gap(self):
        frame = decision_frame(artifacts={"research": {"records": [
            {"url": "https://example.com/listing", "fields": {"rent": {"value": "未取得"}}}]}})
        self.assertEqual(frame["facts"], [])
        self.assertEqual(frame["gaps"], [{"label": "rent", "why": "公开资料未取得"}])

    def test_cost_items_keep_their_basis_and_missing_amounts_stay_gaps(self):
        frame = decision_frame(artifacts={"business_cost": {"items": [
            {"name": "月租", "treatment": "cash_expense", "normalized_amount": 15000,
             "original": {"basis": "assumption"}},
            {"name": "装修", "treatment": "cash_expense", "normalized_amount": None,
             "original": {"basis": "user_provided"}}]}})
        self.assertEqual(frame["assumptions"], [{"label": "月租（现金花费）", "value": "15,000", "via": "分析假设"}])
        self.assertEqual(frame["gaps"], [{"label": "装修（现金花费）", "why": "金额未取得"}])

    def test_multiple_cost_scenarios_are_all_projected(self):
        frame = decision_frame(artifacts={"business_costs": [
            {"items": [{"name": "A店租金", "treatment": "cash_expense", "normalized_amount": 10000, "original": {}}]},
            {"items": [{"name": "B店租金", "treatment": "cash_expense", "normalized_amount": 12000, "original": {}}]}]})
        self.assertEqual([a["value"] for a in frame["assumptions"]], ["10,000", "12,000"])

    def test_task_missing_items_join_the_gap_list_without_duplicating(self):
        frame = decision_frame(artifacts={}, missing=["押金金额未取得", "押金金额未取得"])
        self.assertEqual(frame["gaps"], [{"label": "押金金额未取得", "why": "这次没取得"}])


class FrameResult(unittest.TestCase):
    def test_solved_game_reports_count_and_completeness(self):
        frame = decision_frame(artifacts={"equilibria": {
            "equilibria": [{"id": "e1"}, {"id": "e2"}], "completeness": "Complete"}})
        self.assertEqual(frame["result"], {"equilibria": 2, "completeness": "找全了", "has_recommendation": False})

    def test_partial_completeness_is_not_reported_as_complete(self):
        frame = decision_frame(artifacts={"equilibria": {"equilibria": [{"id": "e1"}],
                                                         "completeness": "Partial"}})
        self.assertEqual(frame["result"]["completeness"], "可能没找全")

    def test_no_equilibria_yet_means_no_result_block(self):
        frame = decision_frame(artifacts={"equilibria": {"equilibria": []}})
        self.assertIsNone(frame["result"])


if __name__ == "__main__":
    unittest.main()


class ChatSnapshotCarriesTheFrame(unittest.TestCase):
    """接线测试：不启动引擎，也能钉住「chat 快照必须带帧」。"""

    def _service(self, project_case=None):
        import tempfile, threading
        from pathlib import Path
        from web_chat import ChatService

        folder = Path(tempfile.mkdtemp()) / "chat-000000000000000000000001"
        folder.mkdir()
        service = ChatService(binary=lambda: "true", home=lambda: "/tmp", playbooks=lambda: [], project_case=project_case)
        entry = dict(id=folder.name, dir=folder, created_at=0, topic="market_entry", mode="whatif",
                     action="跟着降价10%", initial="在上海南京路32号开店值不值", start_key="key-0001",
                     offset=0, case=None, pending=None, finished=None, stopped=False, replies={},
                     proc=None, restored=False, durable_input=False,
                     task_result={"status": "partial", "missing": ["客流"]}, task_scope="consultation",
                     lock=threading.RLock())
        service.entries[entry["id"]] = entry
        return service, entry

    def test_snapshot_has_a_frame_with_the_user_given_action(self):
        service, entry = self._service()
        snapshot = service.get(entry["id"])
        self.assertIn("frame", snapshot)
        self.assertEqual(snapshot["frame"]["type"], "新生意")
        self.assertEqual(snapshot["frame"]["mode"], "试做法")
        self.assertEqual(snapshot["frame"]["scope"], "完整咨询")
        self.assertEqual(snapshot["frame"]["move"]["value"], "跟着降价10%")
        self.assertEqual(snapshot["frame"]["gaps"], [{"label": "客流", "why": "这次没取得"}])

    def test_case_artifacts_feed_players_and_result_into_the_frame(self):
        artifacts = {"gamespec": {"spec": {
            "players": [{"id": "me", "name": "我方", "vn_role": "focal", "relation_to_game": "player"}],
            "strategies": {"me": ["开店", "不开"]}}},
            "equilibria": {"equilibria": [{"id": "e1"}], "completeness": "Complete"}}
        service, entry = self._service(project_case=lambda date, case_id: {"artifacts": artifacts})
        entry["case"] = "2026-09-19/case-20260919-whatif-064437"
        frame = service.get(entry["id"])["frame"]
        self.assertEqual([p["name"] for p in frame["players"]], ["我方"])
        self.assertEqual(frame["players"][0]["moves"], ["开店", "不开"])
        self.assertEqual(frame["result"], {"equilibria": 1, "completeness": "找全了", "has_recommendation": False})

    def test_snapshot_without_a_case_still_has_an_honest_frame(self):
        service, entry = self._service()
        frame = service.get(entry["id"])["frame"]
        self.assertEqual(frame["players"], [], "没有案例就没有玩家，不能编")
        self.assertEqual(frame["facts"], [])


class PoiCountsBecomeVerifiedFacts(unittest.TestCase):
    """周边计数进「已核实」：它们是坐标级事实，带地址、半径与来源。"""

    SURVEY = {
        "address": "上海市黄浦区南京东路", "radius": 500,
        "source": "高德开放平台 Web 服务（POI 周边搜索）",
        "counts": [{"label": "茶饮咖啡", "count": 144}, {"label": "地铁站", "count": 21}],
        "brand": {"label": "喜茶（同品牌）", "count": 3},
        "keywords": {"label": "宠物店（关键词）", "count": 2},
        "unavailable": ["餐饮：CUQPS_HAS_EXCEEDED_THE_LIMIT"],
    }

    def test_counts_are_facts_with_source_and_radius(self):
        frame = decision_frame(artifacts={"poi_surveys": [self.SURVEY]})
        labels = [f["label"] for f in frame["facts"]]
        self.assertEqual(labels, ["周边·茶饮咖啡", "周边·地铁站", "周边·喜茶（同品牌）", "周边·宠物店（关键词）"])
        first = frame["facts"][0]
        self.assertEqual(first["value"], "144 家")
        self.assertIn("南京东路", first["source"])
        self.assertIn("500米", first["source"])

    def test_unavailable_categories_become_gaps_not_zero(self):
        frame = decision_frame(artifacts={"poi_surveys": [self.SURVEY]})
        self.assertEqual(frame["gaps"], [{"label": "周边·餐饮", "why": "这次没取得"}])

    def test_survey_without_counts_still_projects_without_crashing(self):
        frame = decision_frame(artifacts={"poi_surveys": [{"address": "某地", "radius": 500}]})
        self.assertEqual(frame["facts"], [])
        self.assertEqual(frame["gaps"], [])

    def test_two_surveys_are_both_kept(self):
        other = dict(self.SURVEY, address="上海市静安区某路", radius=800,
                     counts=[{"label": "茶饮咖啡", "count": 40}], brand=None, keywords=None, unavailable=[])
        frame = decision_frame(artifacts={"poi_surveys": [self.SURVEY, other]})
        tea = [f for f in frame["facts"] if f["label"] == "周边·茶饮咖啡"]
        self.assertEqual(len(tea), 2, "换了地址再查的结果都要保留")
        self.assertTrue(any("800米" in f["source"] for f in tea))


class LiveProgressCarriesPoiCounts(unittest.TestCase):
    """实时回合：`poi_survey` 的产物必须进快照，否则「看依据」看不到。"""

    def test_progress_events_become_snapshot_facts(self):
        import json as _json
        import tempfile, threading
        from pathlib import Path
        from web_chat import ChatService

        folder = Path(tempfile.mkdtemp()) / "chat-000000000000000000000002"
        folder.mkdir()
        survey = PoiCountsBecomeVerifiedFacts.SURVEY
        (folder / "engine-progress.jsonl").write_text("\n".join([
            _json.dumps({"type": "tool_call_started", "call_id": "c1", "name": "poi_survey"}),
            _json.dumps({"type": "tool_call_finished", "call_id": "c1",
                         "outcome": {"status": "ok", "output": _json.dumps(survey, ensure_ascii=False)}}),
        ]) + "\n", encoding="utf-8")
        service = ChatService(binary=lambda: "true", home=lambda: "/tmp", playbooks=lambda: [])
        entry = dict(id=folder.name, dir=folder, created_at=0, topic="market_entry", mode="scenario",
                     action="", initial="要不要在南京东路32号开店", start_key="key-0002",
                     offset=0, case=None, pending=None, finished=None, stopped=False, replies={},
                     proc=None, restored=False, durable_input=False, task_result=None, task_scope=None,
                     lock=threading.RLock())
        service.entries[entry["id"]] = entry
        snapshot = service.get(entry["id"])
        self.assertEqual(len(snapshot["poi_surveys"]), 1)
        self.assertEqual(snapshot["frame"]["facts"][0]["label"], "周边·茶饮咖啡")
        self.assertEqual(snapshot["frame"]["facts"][0]["value"], "144 家")
