import unittest
from business_cost_projection import merge_cost_scenarios


class CostIdentityTests(unittest.TestCase):
    def test_renaming_or_changing_duration_updates_only_the_same_identity(self):
        old = {"scenario_id": "move", "scenario": "搬店", "horizon_totals": {"months": 12}}
        other = {"scenario_id": "stay", "scenario": "续租"}
        updated = {"scenario_id": "move", "scenario": "搬到新店（两年）", "horizon_totals": {"months": 24}}
        self.assertEqual(merge_cost_scenarios([old, other], updated), [updated, other])

    def test_distinct_ids_and_legacy_labels_are_not_guessed_to_be_the_same(self):
        legacy = {"scenario": "搬店"}
        first = {"scenario_id": "搬店", "scenario": "同名"}
        second = {"scenario_id": "different", "scenario": "同名"}
        self.assertEqual(merge_cost_scenarios([legacy, first, second]), [legacy, first, second])
