"""Project actual search/fetch outcomes without treating snippets as read pages."""
import json
import math
from urllib.parse import urlsplit


def object_value(value):
    if isinstance(value, dict):
        return value
    if isinstance(value, str):
        try:
            result = json.loads(value)
        except ValueError:
            return {}
        return result if isinstance(result, dict) else {}
    return {}


def safe_url(value):
    if not isinstance(value, str):
        return None
    if any(ord(char) <= 32 or ord(char) == 127 for char in value):
        return None
    try:
        parsed = urlsplit(value)
        if parsed.scheme in ("http", "https") and parsed.netloc and not parsed.username and not parsed.password:
            return value
    except ValueError:
        pass
    return None


class WebSources:
    def __init__(self):
        self.calls = {}
        self.sources = {}
        self.aliases = {}

    def source(self, url):
        seen = set()
        while url in self.aliases and url not in seen:
            seen.add(url)
            url = self.aliases[url]
        return self.sources.setdefault(url, {"url": url, "title": urlsplit(url).netloc, "status": "search_only"})

    def feed(self, event):
        if event.get("type") == "tool_call_started":
            self.calls[event.get("call_id")] = event
            return
        if event.get("type") != "tool_call_finished":
            return
        call = self.calls.get(event.get("call_id"), {})
        name = event.get("name") or call.get("name")
        result = event.get("outcome", {})
        output = object_value(result.get("output"))
        if name == "web_search" and result.get("status") == "ok":
            hits = output.get("hits", [])
            for hit in hits if isinstance(hits, list) else []:
                if not isinstance(hit, dict) or not safe_url(hit.get("url")):
                    continue
                source = self.source(hit["url"])
                if isinstance(hit.get("title"), str) and hit["title"].strip():
                    source["title"] = hit["title"]
            return
        if name != "web_fetch":
            return
        requested = safe_url(object_value(call.get("arguments")).get("url"))
        actual = safe_url(output.get("url"))
        if not (actual or requested):
            return
        read = (result.get("status") == "ok" and isinstance(output.get("text"), str)
                and bool(output["text"].strip())
                and ("url" not in output or actual is not None))
        # A failed/empty response cannot prove that the old and returned URLs
        # identify the same readable page. Keep the prior snapshot at its URL.
        url = (actual or requested) if read else (requested or actual)
        if read and actual:
            # An explicitly returned URL identifies this retrieved snapshot;
            # an older redirect mapping must not relabel its contents.
            self.aliases.pop(actual, None)
        if read and requested and requested != url:
            prior = self.sources.pop(requested, None)
            self.aliases[requested] = url
            if prior and url not in self.sources:
                self.sources[url] = {**prior, "url": url}
        source = self.source(url)
        if read:
            stamp = output.get("retrieved_at_unix")
            if (isinstance(stamp, bool) or not isinstance(stamp, (int, float))
                    or not math.isfinite(stamp) or stamp < 0):
                stamp = None
            previous_stamp = source.get("retrieved_at_unix")
            cached = output.get("cached") is True
            source["status"] = "body_retrieved"
            if cached and stamp is not None and previous_stamp is not None and stamp < previous_stamp:
                source["last_replayed_snapshot_at_unix"] = stamp
            else:
                source["retrieved_at_unix"] = stamp
                source["cached"] = cached
                source.pop("last_replayed_snapshot_at_unix", None)
            # Replaying a snapshot does not repair a failed attempt to refresh it.
            if not cached:
                source["last_attempt_failed"] = False
            else:
                source.setdefault("last_attempt_failed", False)
        else:
            if source["status"] != "body_retrieved":
                source["status"] = "body_unavailable"
            source["last_attempt_failed"] = True

    def records(self):
        return sorted(self.sources.values(), key=lambda source: source["status"] != "body_retrieved")


def project_web_sources(events):
    projection = WebSources()
    for event in events:
        projection.feed(event)
    return projection.records()
