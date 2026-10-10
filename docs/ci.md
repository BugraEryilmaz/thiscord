# CI builds and caches

CI runs on pull requests, using the existing self-hosted Windows, Linux and macOS
runner labels. Pushes to `main` and manual dispatch do not currently trigger it.
Formatting, backend/database checks, WASM, and the three-platform audio matrix
start independently. Desktop builds wait for WASM assets. Client release tags run
the separate release workflow: validate versions, build WASM, build installers,
then verify and publish. That workflow does not rerun the PR test suite; only tag
a commit whose checks have passed. Signed artifacts and the complete platform feed
are verified before publication.

## Development and worktrees

The pinned Cargo supports separate intermediate and final output directories.
`.cargo/config.toml` sets `build.build-dir = "{cargo-cache-home}/thiscord"`.
Normal Cargo, Trunk, Tauri and both VS Code workspaces therefore reuse one
intermediate directory per user/Cargo home, across this repository's checkouts:

- Windows: `%USERPROFILE%\.cargo\thiscord` by default.
- Linux/WSL: `~/.cargo/thiscord` on the Linux filesystem.
- macOS: `~/.cargo/thiscord` on that Mac.

This shares compiled dependencies, build-script output and incremental state.
Final executables, WASM and bundles remain in each checkout's `target` directory,
or its explicit `CARGO_TARGET_DIR`. `frontend/dist` remains checkout-local too.
An executable from one worktree cannot be replaced by another worktree's build.
Use separate Cargo homes on Windows and WSL. For WSL commands on a Windows-mounted
checkout, continue setting `CARGO_TARGET_DIR="$HOME/.cache/thiscord-target"` to
keep final Linux output separate; `backend/run.sh` sets this default automatically
and honors an existing override.

Cargo locks the shared intermediate directory while building. Editors and terminal
builds can wait for each other; the two editor workspaces retain their different
package/feature/target selections but no longer create separate compiler caches.
If concurrent compilation is necessary, assign a small fixed set of additional
slots with `CARGO_BUILD_BUILD_DIR` (for example `~/.cargo/thiscord-slot2`) in that
terminal and any editor launched from it. Reuse those slots, rather than naming
them after tasks or branches. Do not point concurrent checkouts at the same final
`CARGO_TARGET_DIR`; Cargo's build lock does not protect later executable/bundle use.

New clones/worktrees inherit the setting once they contain this configuration.
Older branches retain their old configuration until updated. For an existing
parent directory dedicated solely to Thiscord worktrees, the same `[build]`
setting can be placed in that parent's `.cargo/config.toml`; Cargo's ancestor
configuration then covers its older worktrees too. Do not put it in a machine-wide
Cargo configuration, which would also affect unrelated Rust projects.

An explicit `--target-dir` or `CARGO_TARGET_DIR` only changes final output after
this change. Set `CARGO_BUILD_BUILD_DIR` as well when isolating all build output.
`cargo clean` can clear the shared intermediate cache, affecting all its users;
avoid routine clean builds. The initial switch can require a cold build. Existing
target trees are not moved or deleted automatically.

No compiler wrapper is required. `sccache` remains a possible measured follow-up
for many concurrent slots; it does not cache every Rust invocation or final link
and requires disabling incremental compilation for cached invocations. This
configuration preserves Cargo's incremental development builds without adding
another tool or a second copy of compiler-cache data.

## Persistent self-hosted runners

The local `cargo-cache` action detects `runner.environment == self-hosted` and
sets both `CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR` to one persistent slot
alongside the runner's temporary directory, outside checkout/temp cleanup. Roots
are `_work/tc/<32-character hash>` on Windows (short for MSVC/CMake paths) and
`_work/thiscord-cargo/<64-character hash>` on Linux/macOS.

The identity includes repository, runner service name, OS, architecture and build
environment. It excludes PR number, branch, tag, job and lockfile hash. Backend,
WASM, desktop, audio and release verification share the `native` slot on a runner;
the Ubuntu 24.04 installer container has a separate `ubuntu24-container` slot.
Cargo handles source, toolchain, feature, profile and dependency invalidation.
There is one native slot per runner service, plus one container slot on each Linux
runner that builds installers. Concurrent runner services must have unique names.
GitHub runs one job at a time on each service, so jobs and their artifact consumers
do not overwrite each other's output concurrently.

The installer container explicitly sets both directories to `/cache/target` and
keeps its Cargo home in the stable slot's `/cache/cargo-home`. Container registry
downloads are reused across release tags/PRs; host and container Cargo homes remain
separate to avoid importing host configuration and installed binaries into Ubuntu.

Self-hosted jobs keep local compiled dependencies, workspace artifacts, and Cargo
incremental state without downloading/uploading the target directory. Normal
Cargo registry/git downloads and Rust toolchains also remain in the runner user's
home. A fresh runner or build environment still needs an initial cold build.
Runner routing and available parallel slots determine the wall-clock benefit;
splitting jobs does not give a single runner service extra execution slots.

The actions use PowerShell 7 (`pwsh`) on all platforms, as the release scripts do.
The format job runs `python3 scripts/test-cargo-cache.py`, which exercises the real
selectors, builds two disposable Cargo checkouts to verify dependency reuse and
binary isolation, and tests cleanup using temporary directories.
Keep the workflow's native system dependencies installed on each runner. Linux
jobs still install/check their declared packages so provisioning drift fails
visibly rather than being hidden by a warm Rust cache.

Set repository variable `THISCORD_DISABLE_LOCAL_CACHE=true` to use GitHub caching
instead, for example on ephemeral self-hosted runners. This also provides a simple
way to compare local and remote cache performance.

Hosted runners and that opt-out use `Swatinem/rust-cache`, retaining workspace
crates and saving reusable compilation work on failed checks. Both final outputs
and intermediates live in `target` for this fallback, so restore/save covers both.
Pinned Trunk, Diesel and Tauri CLIs use separate version/platform/toolchain/feature
identities. On persistent runners their installed binaries remain under
`_work/thiscord-tools` without a GitHub cache transfer on each job; hosted/opt-out
runners continue using `actions/cache`. Cargo verifies the exact installed version.
CLI cache updates do not depend on the application Cargo.lock. Existing PR
restrictions on GitHub cache access still apply. Cache directory isolation has
never been a security sandbox for untrusted code on a persistent runner.

## Retiring old caches

Current slots have a `thiscord-cache.json` marker and are always retained by the
cleanup script. The number of slots is bounded by runner services/environments,
but files for old toolchains/features inside a slot can still accumulate. Inspect
disk use periodically; there is no background deletion that could disrupt builds.

Preview legacy per-PR/job cache directories older than 30 days:

```powershell
./scripts/prune-cargo-cache.ps1 -CacheRoot C:\thiscord-runner\_work\tc
# Linux example (PowerShell 7):
pwsh -File scripts/prune-cargo-cache.ps1 -CacheRoot /home/bugra/thiscord-runner/_work/thiscord-cargo
```

Stop every runner service and build using that root before applying cleanup, and
keep them stopped until it finishes. `-RunnersStopped` is an operator assertion,
not an automatic service stop or a lock. Then repeat with `-Apply -RunnersStopped`.
`-MinimumAgeDays` can change the retention period (minimum one day). The script
only considers direct hash-named children, preserves marked slots and recently
written trees, and refuses symlinks/junctions or paths outside the selected root.
It does not delete source worktrees, Cargo homes, GitHub artifacts, tool installs,
or development targets. Older workflow revisions can recreate legacy caches until
those branches are updated.

Historical worktree targets and tool-version directories require a separate,
reviewed cleanup after their consumers are stopped. Do not blindly move or merge
old `deps`, `build` or `incremental` trees: build scripts can embed absolute paths.

## Compilation profiles and measurement

Functional audio tests and audio Clippy use `--profile ci`: release optimization
without LTO, with 16 codegen units. They use the same audio/probe feature graph.
This preserves optimized neural inference without paying production linking cost
for functional checks. Audio benchmark executables are built together with the
normal `release` profile; benchmark artifacts remain available on all platforms.
Production WASM and signed installers also retain thin LTO and one codegen unit.

Compare a second successful run on the same runners with the first cold run.
Inspect compilation, CLI installation, cache transfer, job queue, and artifact
upload durations separately. Local cache selection and the target directory are
printed in each job. The workflow changes do not establish a measured speedup
until they have run on the actual fleet.
