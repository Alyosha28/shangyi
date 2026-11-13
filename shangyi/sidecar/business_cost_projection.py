"""Preserve the latest successful cost calculation for each named scenario."""


def merge_cost_scenarios(*groups):
    """Keep first-seen display order; an unnamed legacy calculation has one slot."""
    scenarios = {}
    for group in groups:
        if isinstance(group, dict):
            group = [group]
        for cost in group or []:
            if not isinstance(cost, dict):
                continue
            scenario = cost.get("scenario")
            identifier = cost.get("scenario_id")
            key = ("id", identifier) if isinstance(identifier, str) and identifier else (
                "legacy", scenario.strip() if isinstance(scenario, str) else "")
            scenarios[key] = cost
    return list(scenarios.values())


def cost_scenarios(artifacts):
    """Accept both current scenario collections and historical single artifacts."""
    return merge_cost_scenarios(artifacts.get("business_costs"), artifacts.get("business_cost"))


def record_business_cost(artifacts, cost):
    if not isinstance(cost, dict):
        return
    artifacts["business_costs"] = merge_cost_scenarios(cost_scenarios(artifacts), cost)
    # Keep the old field as the last successful call for existing API consumers.
    artifacts["business_cost"] = cost
