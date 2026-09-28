#Requires -RunAsAdministrator
$ErrorActionPreference = 'Stop'
foreach ($rule in @(
    @{ Name = 'Thiscord-TURN-UDP'; Protocol = 'UDP'; Ports = @('3478', '49160-49259') },
    @{ Name = 'Thiscord-TURN-TCP'; Protocol = 'TCP'; Ports = @('3478') }
)) {
    if (!(Get-NetFirewallRule -Name $rule.Name -ErrorAction SilentlyContinue)) {
        New-NetFirewallRule -Name $rule.Name -DisplayName $rule.Name -Direction Inbound -Action Allow -Protocol $rule.Protocol -LocalPort $rule.Ports -Profile Any | Out-Null
    }
}
Write-Output 'Thiscord TURN listener and relay firewall rules are present.'
