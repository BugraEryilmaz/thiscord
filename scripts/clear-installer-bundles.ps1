# Preserve compiled dependencies; only remove generated installer archives.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$TargetDirectory,
    [Parameter(Mandatory)]
    [ValidateSet('x86_64-pc-windows-msvc', 'x86_64-unknown-linux-gnu', 'aarch64-apple-darwin')]
    [string]$Target
)
$ErrorActionPreference = 'Stop'
$bundle = Join-Path ([IO.Path]::GetFullPath($TargetDirectory)) "$Target/release/bundle"
$outputs = switch ($Target) {
    'x86_64-pc-windows-msvc' { @{ nsis = @('.exe', '.exe.sig') } }
    'x86_64-unknown-linux-gnu' { @{ deb = @('.deb', '.deb.sig'); appimage = @('.AppImage', '.AppImage.sig') } }
    'aarch64-apple-darwin' { @{ dmg = @('.dmg'); macos = @('.app.tar.gz', '.app.tar.gz.sig') } }
}
foreach ($directory in $outputs.Keys) {
    $path = Join-Path $bundle $directory
    if (!(Test-Path -LiteralPath $path)) { continue }
    foreach ($file in Get-ChildItem -LiteralPath $path -File) {
        foreach ($extension in $outputs[$directory]) {
            if ($file.Name.EndsWith($extension, [StringComparison]::Ordinal)) {
                Remove-Item -LiteralPath $file.FullName -Force
                break
            }
        }
    }
}
