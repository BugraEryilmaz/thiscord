"""Run with python scripts/test-select-runners.py."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import urllib.error

spec = importlib.util.spec_from_file_location("routing", Path(__file__).with_name("select-runners.py"))
routing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(routing)


def runner(labels, status="online", busy=False):
    return {"status": status, "busy": busy, "labels": [{"name": s} for s in labels]}


class RoutingTests(unittest.TestCase):
    def test_platform_architecture_and_availability(self):
        routes = routing.select([
            runner(["self-hosted", "windows", "x64"]),
            runner(["self-hosted", "macOS", "X64"]),
            runner(["self-hosted", "Linux", "X64"], status="offline"),
            runner(["self-hosted", "macOS", "ARM64"], status="offline"),
        ])
        self.assertEqual(routes["windows-latest"], routing.PLATFORMS["windows-latest"])
        self.assertEqual(routes["macos-latest"], ["macos-latest"])
        self.assertEqual(routes["ubuntu-24.04"], ["ubuntu-24.04"])

    def test_all_platforms_and_case_insensitive_labels(self):
        runners = [runner([s.lower() for s in labels]) for labels in routing.PLATFORMS.values()]
        self.assertEqual(routing.select(runners),
                         {**routing.PLATFORMS, "linux-installer": ["ubuntu-24.04"]})

    def test_busy_online_runners_remain_eligible(self):
        runners = [runner(labels, busy=True) for labels in routing.PLATFORMS.values()]
        self.assertEqual(routing.select(runners),
                         {**routing.PLATFORMS, "linux-installer": ["ubuntu-24.04"]})

    def test_installers_require_verified_distribution_label(self):
        kali = runner(["self-hosted", "Linux", "X64", "ubuntu-24.04"])
        self.assertEqual(routing.select([kali])["linux-installer"], ["ubuntu-24.04"])
        ubuntu = runner(routing.INSTALLER_LABELS)
        self.assertEqual(routing.select([kali, ubuntu])["linux-installer"], routing.INSTALLER_LABELS)
        ubuntu["busy"] = True
        self.assertEqual(routing.select([kali, ubuntu])["linux-installer"], routing.INSTALLER_LABELS)
        ubuntu["status"] = "offline"
        self.assertEqual(routing.select([kali, ubuntu])["linux-installer"], ["ubuntu-24.04"])

    def test_missing_token_pr_and_api_failure_fall_back(self):
        for event, token in [("push", ""), ("pull_request", "test"), ("push", "test")]:
            with self.subTest(event=event, token_present=bool(token)), tempfile.TemporaryDirectory() as tmp:
                output = Path(tmp) / "output"
                env = {"GITHUB_EVENT_NAME": event, "RUNNER_STATUS_TOKEN": token,
                       "GITHUB_OUTPUT": str(output), "GITHUB_STEP_SUMMARY": str(Path(tmp) / "summary")}
                with patch.dict(os.environ, env), patch.object(
                    routing, "fetch_runners", side_effect=urllib.error.URLError("unavailable")
                ) as fetch:
                    routing.main()
                self.assertEqual(fetch.call_count, int(event == "push" and bool(token)))
                self.assertEqual(json.loads(output.read_text().split("=", 1)[1]),
                                 {**{platform: [platform] for platform in routing.PLATFORMS},
                                  "linux-installer": ["ubuntu-24.04"]})

    def test_paginated_lookup(self):
        from io import BytesIO
        first = [runner(["self-hosted", "Linux", "X64"], status="offline")] * 100
        last = [runner(["self-hosted", "Linux", "X64"])]
        with patch.dict(os.environ, {"GITHUB_API_URL": "https://api.github.com", "GITHUB_REPOSITORY": "owner/repo"}), patch.object(
            routing.urllib.request, "urlopen",
            side_effect=[BytesIO(json.dumps({"runners": first}).encode()), BytesIO(json.dumps({"runners": last}).encode())]
        ) as fetch:
            result = routing.fetch_runners("test")
        self.assertEqual(fetch.call_count, 2)
        self.assertEqual(routing.select(result)["ubuntu-24.04"], routing.PLATFORMS["ubuntu-24.04"])


if __name__ == "__main__":
    unittest.main()
