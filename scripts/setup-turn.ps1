[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$PublicIp,
    [Parameter(Mandatory)][string]$SfuIp,
    [string]$Hostname = 'thiscord.com.tr'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
foreach ($address in @($PublicIp, $SfuIp)) {
    $parsed = [Net.IPAddress]::Parse($address)
    if ($parsed.AddressFamily -ne [Net.Sockets.AddressFamily]::InterNetwork -or $parsed.ToString() -cne $address) {
        throw 'Use canonical IPv4 addresses.'
    }
}
if ($Hostname -cnotmatch '^[a-z0-9]+([.-][a-z0-9]+)*$') { throw 'Invalid hostname.' }
$environment = Join-Path $root 'backend/.env'
if (!(Test-Path -LiteralPath $environment)) { throw 'Create backend/.env first.' }
$contents = [IO.File]::ReadAllText($environment)
$secretMatch = [regex]::Match($contents, '(?m)^THISCORD_TURN_SECRET=([a-f0-9]{64})\r?$')
if ($secretMatch.Success) {
    $secret = $secretMatch.Groups[1].Value
} elseif ($contents -match '(?m)^THISCORD_TURN_SECRET=') {
    throw 'An existing TURN secret is present. Preserve it and configure coturn manually; this script will not rotate it.'
} else {
    $bytes = New-Object byte[] 32
    $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
    try { $rng.GetBytes($bytes) } finally { $rng.Dispose() }
    $secret = -join ($bytes | ForEach-Object { $_.ToString('x2') })
}
$configPath = Join-Path $root 'infra/coturn/turnserver.conf'
$config = [IO.File]::ReadAllText("$configPath.example").Replace('REPLACE_WITH_THE_BACKEND_THISCORD_TURN_SECRET', $secret).Replace('REPLACE_WITH_PUBLIC_IPV4', $PublicIp).Replace('REPLACE_WITH_SFU_WSL_IPV4', $SfuIp).Replace('thiscord.com.tr', $Hostname)
$utf8 = New-Object Text.UTF8Encoding($false)
# Restrict the private config before writing credentials (Windows checkout).
if (!(Test-Path -LiteralPath $configPath)) { [IO.File]::WriteAllText($configPath, '', $utf8) }
$acl = Get-Acl -LiteralPath $configPath
if (!$acl.AreAccessRulesProtected) {
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($identity in @([Security.Principal.WindowsIdentity]::GetCurrent().Name, 'NT AUTHORITY\SYSTEM')) {
        $acl.SetAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($identity, 'FullControl', 'Allow')))
    }
    Set-Acl -LiteralPath $configPath -AclObject $acl
}
$allowedSids = @([Security.Principal.WindowsIdentity]::GetCurrent().User.Value, 'S-1-5-18')
foreach ($rule in (Get-Acl -LiteralPath $configPath).Access) {
    $sid = $rule.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value
    if ($rule.AccessControlType -eq 'Allow' -and $sid -notin $allowedSids) {
        throw 'Private coturn config grants another identity access. Restrict its ACL before writing secrets.'
    }
}
[IO.File]::WriteAllText($configPath, $config, $utf8)
foreach ($entry in @{
    THISCORD_STUN_URL = "stun:${Hostname}:3478"
    THISCORD_TURN_URL = "turn:${Hostname}:3478?transport=udp"
    THISCORD_TURN_SECRET = $secret
}.GetEnumerator()) {
    $pattern = '(?m)^' + [regex]::Escape($entry.Key) + '=.*\r?$'
    $line = $entry.Key + '=' + $entry.Value
    if ([regex]::IsMatch($contents, $pattern)) { $contents = [regex]::Replace($contents, $pattern, $line) }
    else { $contents = $contents.TrimEnd() + "`n" + $line + "`n" }
}
[IO.File]::WriteAllText($environment, $contents, $utf8)
Write-Output 'Private coturn config and backend STUN/TURN settings written. Existing provider settings were preserved.'
Write-Output 'Start with: docker compose -f infra/coturn/compose.yml up -d'
Write-Output 'Restart the backend after verifying connectivity. Do not print the private configuration.'
