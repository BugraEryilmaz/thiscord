# CI builds and caches

CI runs on pull requests, using the existing self-hosted Windows, Linux and macOS
runner labels. Pushes to `main` and manual dispatch do not currently trigger it.
Formatting, backend/database checks, WASM, and the three-platform audio matrix
start independently. Desktop builds wait for WASM assets. Client release tags run
the separate release workflow: validate versions, build WASM, build installers,
then verify and publish. That workflow does not rerun the PR test suite; only tag
a commit whose checks have passed. Signed artifacts and the complete platform feed
are verified before publication.

Backend integration tests run with `--include-ignored --test-threads=4`. Each voice
fixture owns a five-connection pool and a schema-management connection; unrestricted
test parallelism can exhaust PostgreSQL's connection limit as the suite grows. Use
the same bound for local combined database/media runs. Separate concurrent Cargo
test processes still share the database's connection budget.

## Linux web optimizer workaround

The Linux CI standard/admin web steps and the Linux release asset step set
`BINARYEN_CORES=1` in their **step environment**, inherited by Trunk and its
`wasm-opt` child. Cargo's `[env]` cannot configure its parent Trunk process.
This keeps the existing `-O` optimization and tool versions; it only serializes
Binaryen work. Installer jobs consume those assets and disable Tauri's build
hook, so they do not run another optimizer. Windows/macOS builds and machine-wide
environment settings are unchanged. For a local Linux reproduction use
`BINARYEN_CORES=1 trunk build --release --locked` (and the usual admin arguments).

This is a workaround for observed multithreaded optimizer failures, not a proven
upstream root-cause fix. In runs `39331053849` (PR 53) and `39330549170` (PR 52),
Rust and Tailwind succeeded before Binaryen 123 crashed with SIGSEGV. PR 52's
second attempt passed the standard bundle, then failed the admin bundle. The
runner's retained wasm-bindgen input was copied to temporary storage before
standalone testing: 41,775,405 bytes, SHA-256
`7456272bad2fc1f1a5fd26a6b84579c4892698aca8822a7190ab1eaa21252566`.
It was timestamped at the second attempt's admin build, not PR 53's earlier build.
The cached executable matched the freshly downloaded official version 123
Linux x86_64 executable byte for byte, SHA-256
`d66c6724c07334155720eb2def29c434dcaaf741ce859b4f7f389e22674f9c4a`.

With immutable input and separate temporary outputs, default concurrency failed
three of three bounded trials with SIGSEGV (another preliminary trial raised
SIGILL). All three single-worker trials passed in 17.10-20.66 seconds, producing
byte-identical output with SHA-256
`b8490523f61deb23b0af1323fb573f8d56907d4b7a069f715ff36a144f74242d`.
The earlier serial/debug runs and three two-worker trials also produced that
output. One worker is chosen to bypass the worker pool completely; two workers
passing this small sample does not establish a generally safe concurrency limit.
Increasing the stack size on a temporary executable copy did not fix the crash;
official version 130 also crashed, so neither a stack diagnosis nor a version
upgrade is justified by this evidence. Initial measured trials used under
0.5 GiB peak RSS with several GiB available; no OOM kill was observed. This
isolates a concurrency-sensitive tool/runner failure, but does not establish
its underlying defect.

Binaryen's [version 123 thread implementation](https://github.com/WebAssembly/binaryen/blob/version_123/src/support/threads.cpp)
reads `BINARYEN_CORES`; one core bypasses its worker pool. Trunk's
[pinned command runner](https://github.com/trunk-rs/trunk/blob/v0.21.14/src/common/mod.rs)
inherits the environment. Upstream's [Alpine stack fix](https://github.com/WebAssembly/binaryen/pull/8595)
describes a different possible cause; our enlarged-stack experiment did not
resolve this case. Reassess the workaround only with the same input, verified
tool provenance and repeated successful optimized builds; do not clear caches
or disable optimization to hide the failure.

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

Self-hosted jobs keep local compiled dependencies and workspace artifacts without
downloading/uploading the target directory. All three workflows set
`CARGO_INCREMENTAL=0`; the Linux installer container sets it explicitly too.
Unchanged dependencies remain reusable, but changed crates do not retain rustc's
additional incremental state. Normal
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

## Build storage maintenance

Every build job reports profile and incremental sizes at the end, including failed
jobs when the cache was selected. `scripts/maintain-cargo-builds.ps1` also reports
development caches. Sizes are logical file bytes, not unique filesystem blocks;
hard links, compression and filesystem snapshots can change actual reclaimed space.
The default budget is 40 GiB across known build profiles in one slot. Exceeding it
prints a warning, never deletes files or fails an otherwise successful build.
Registry downloads, tools and files outside those profiles are excluded from this
budget. Container profiles under `target/` are included.

Review reports weekly and after Rust/toolchain or dependency upgrades. During a
maintenance window, stop all runner services and development/editor builds using
the selected root, and wait for artifact uploads, signing and running binaries to
finish. Keep consumers stopped throughout inspection and deletion. `-BuildsStopped`
is an operator assertion, not an automatic stop or a lock. The tool rejects linked
roots/ancestors and links inside any selected deletion tree before deleting anything.
Reports count omitted links without following them; retained installer bundles can
contain links without preventing cleanup of unrelated debug/incremental trees.

```powershell
# Read-only report for one current slot (use its printed CARGO_TARGET_DIR).
./scripts/maintain-cargo-builds.ps1 -BuildRoot C:\thiscord-runner\_work\tc\<slot>
# One-time removal of old CI incremental state after disabling incremental builds.
./scripts/maintain-cargo-builds.ps1 -BuildRoot C:\thiscord-runner\_work\tc\<slot> -Mode Incremental -MinimumAgeDays 0
# Development: preview only crate incremental directories untouched for 14 days.
./scripts/maintain-cargo-builds.ps1 -BuildRoot "$env:USERPROFILE/.cargo/thiscord" -Mode Incremental
# If still over budget, preview resetting complete debug trees in the slot.
./scripts/maintain-cargo-builds.ps1 -BuildRoot C:\thiscord-runner\_work\tc\<slot> -Mode ResetDebug
```

Replace `<slot>` with the actual directory. Run the same script with `pwsh` on
Linux/macOS and their absolute paths. After reviewing the preview with consumers
stopped, repeat the chosen cleanup command with `-Apply -BuildsStopped`.
`Incremental` preserves compiled dependencies and executables. `ResetDebug` removes
whole `debug` profile trees only when the total exceeds `-MaximumGiB` (default 40),
including host and WASM debug output. It preserves release/ci profiles, slot markers,
Cargo homes and tools. It requires a cold debug rebuild; do not run it after every
job. `-MaximumGiB 0` explicitly forces a one-time debug reset, for example after
switching debug-information settings. Changing settings alone does not delete old
artifacts. Use the separate legacy pruning script for obsolete hash-named slots.
No unattended deletion is scheduled. Older branches can recreate incremental or
legacy artifacts until they adopt these settings. Cargo's download-cache garbage
collection does not clean compiled build artifacts.

## Compilation profiles and measurement

Development and test profiles use `debug = "line-tables-only"` for file/line
backtraces without full variable/type debug information. Development incremental
compilation stays enabled. For a debugger session needing variables, override both
`CARGO_PROFILE_DEV_DEBUG=2` and `CARGO_PROFILE_TEST_DEBUG=2` in that terminal; this
creates another artifact variant. Debug assertions, overflow checks and the existing
DSP optimization overrides are unchanged. CI disables incremental compilation through
its workflow environment rather than changing local development defaults.

Functional audio tests and audio Clippy use `--profile ci`: release optimization
without LTO, with 16 codegen units. They use the same audio/probe feature graph.
This preserves optimized neural inference without paying production linking cost
for functional checks. Audio benchmark executables are built together with the
normal `release` profile; benchmark artifacts remain available on all platforms.
The DTX benchmark artifact compares continuous and suppressed microphone packet
counts, payload/header bytes and codec-loop cost for synthetic silence/talkspurts.
It excludes DSP, devices and encrypted/network overhead. Audio tests also retain
a real 31-second silent interval across two encrypted hops to exercise transport
keepalives; physical NAT/TURN and acoustic acceptance remain separate.
Production WASM and signed installers also retain thin LTO and one codegen unit.

Compare a second successful run on the same runners with the first cold run.
Inspect compilation, CLI installation, cache transfer, job queue, and artifact
upload durations separately. Local cache selection and the target directory are
printed in each job. The workflow changes do not establish a measured speedup
until they have run on the actual fleet.
