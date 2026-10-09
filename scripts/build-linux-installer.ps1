$ErrorActionPreference = 'Stop'
Set-Location /workspace
if (!(Test-Path frontend/dist/index.html)) { throw 'Production WASM assets are missing.' }
if (!$env:TAURI_SIGNING_PRIVATE_KEY -or !$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) {
    throw 'Updater signing secrets are required.'
}
Push-Location frontend
try {
    ../scripts/clear-installer-bundles.ps1 -TargetDirectory $env:CARGO_TARGET_DIR -Target x86_64-unknown-linux-gnu
    '{"build":{"beforeBuildCommand":""}}' | Set-Content tauri.ci.conf.json
    cargo tauri build --ci --target x86_64-unknown-linux-gnu --config tauri.ci.conf.json -- --locked
    if ($LASTEXITCODE) { throw 'Linux installer build failed.' }
} finally { Pop-Location }
./scripts/release-client.ps1 -Mode Collect -Tag $env:RELEASE_TAG -Target x86_64-unknown-linux-gnu -Platform linux-x86_64
