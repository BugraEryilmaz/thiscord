# CI builds and caches

CI runs on pull requests, pushes to `main`, and manual dispatch. Pushes to other
branches are checked when a PR is opened or updated; use manual dispatch to check
a branch without a PR. Existing runner labels are unchanged.

Formatting, backend/database checks, WASM, and the three-platform audio matrix
start independently. Desktop builds wait for WASM assets. Client release tags
enable the installer matrix in the same reusable CI workflow, so installers can
start after WASM rather than waiting for desktop/audio/backend checks. The release
publisher waits for the entire reusable workflow, including every installer.
Signed artifacts and the complete platform feed are still verified before publish.

## Persistent self-hosted runners

The local `cargo-cache` action detects `runner.environment == self-hosted` and
sets `CARGO_TARGET_DIR` to a directory under `thiscord-cargo`, alongside the
runner's temporary directory (outside checkout and temporary-directory cleanup).
It separates directories by repository, runner service name, job/platform, and
PR number. Branch/tag builds share a warm directory for each job. Cargo handles
source, toolchain, profile, and dependency invalidation. Concurrent runner services
must have unique names. Windows and Linux/WSL runners retain separate directories.

Self-hosted jobs keep local compiled dependencies, workspace artifacts, and Cargo
incremental state without downloading/uploading the target directory. Normal
Cargo registry/git downloads and Rust toolchains also remain in the runner user's
home. A fresh runner or newly assigned job still needs an initial cold build.
Runner routing and available parallel slots determine the wall-clock benefit;
splitting jobs does not give a single runner service extra execution slots.

The actions use PowerShell 7 (`pwsh`) on all platforms, as the release scripts do.
Keep the workflow's native system dependencies installed on each runner. Linux
jobs still install/check their declared packages so provisioning drift fails
visibly rather than being hidden by a warm Rust cache.

Monitor disk usage under `thiscord-cargo`; it is not automatically pruned. Remove
obsolete PR/job cache directories only while their runner services are idle.
Set repository variable `THISCORD_DISABLE_LOCAL_CACHE=true` to use GitHub caching
instead, for example on ephemeral self-hosted runners. This also provides a simple
way to compare local and remote cache performance.

Hosted runners and that opt-out use `Swatinem/rust-cache`, retaining workspace
crates and saving reusable compilation work on failed checks. Pinned Trunk, Diesel,
and Tauri CLIs use separate version/platform/toolchain/feature-keyed caches; Cargo
verifies the exact installed version. CLI cache updates do not depend on the
application Cargo.lock. Existing PR restrictions on GitHub cache access still apply.
Separating PR target directories is cache hygiene, not a security sandbox for
untrusted code on a persistent runner.

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
