"""Initial runner selection. API failures deliberately fall back to hosted CI."""
import json
import os
import urllib.error
import urllib.request


PLATFORMS = {
    "windows-latest": ["self-hosted", "Windows", "X64"],
    "macos-latest": ["self-hosted", "macOS", "ARM64"],
    "ubuntu-24.04": ["self-hosted", "Linux", "X64"],
}


def select(runners):
    routes = {}
    for hosted, required in PLATFORMS.items():
        eligible = any(
            r.get("status") == "online"
            and r.get("busy") is False
            and set(s.lower() for s in required).issubset(
                label["name"].lower() for label in r.get("labels", [])
            )
            for r in runners
        )
        routes[hosted] = required if eligible else [hosted]
    return routes


def fetch_runners(token):
    runners = []
    # Bound API work, including pagination; never print authentication or bodies.
    for page in range(1, 11):
        url = (
            f"{os.environ['GITHUB_API_URL']}/repos/{os.environ['GITHUB_REPOSITORY']}"
            f"/actions/runners?per_page=100&page={page}"
        )
        request = urllib.request.Request(url, headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
        })
        with urllib.request.urlopen(request, timeout=10) as response:
            batch = json.load(response)["runners"]
        runners.extend(batch)
        if len(batch) < 100:
            return runners
    raise ValueError("Runner pagination limit exceeded")


def main():
    runners = []
    token = os.environ.get("RUNNER_STATUS_TOKEN", "")
    # Pull requests never execute untrusted code on persistent personal machines
    # or receive the runner-administration credential.
    if os.environ.get("GITHUB_EVENT_NAME") == "pull_request":
        print("Pull request: using GitHub-hosted runners.")
    elif not token:
        print("::warning::RUNNER_STATUS_TOKEN is missing; using GitHub-hosted runners.")
    else:
        try:
            runners = fetch_runners(token)
        except (urllib.error.URLError, TimeoutError, ValueError, KeyError, TypeError):
            print("::warning::Runner availability lookup failed; using GitHub-hosted runners.")
    routes = select(runners)
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write("routes=" + json.dumps(routes, separators=(",", ":")) + "\n")
    with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as summary:
        summary.write("### Initial runner selection\n\n| Platform | Labels |\n|---|---|\n")
        for platform, labels in routes.items():
            summary.write(f"| {platform} | `{', '.join(labels)}` |\n")


if __name__ == "__main__":
    main()
