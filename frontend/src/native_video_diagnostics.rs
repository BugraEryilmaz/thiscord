//! Read-only report for remote video playback support. Collects versions,
//! policies, adapter/firewall summaries and bridge records; never addresses,
//! SDP, credentials, file contents or media.
use tauri::Manager;
use thiscord_shared::video_bridge::{Report, Section};

pub async fn collect(app: tauri::AppHandle) -> Result<Report, String> {
    let records = thiscord_frontend::screen::bridge::records().await;
    let mut streams = app
        .state::<crate::native_screen::ScreenState>()
        .stream_sections();
    streams.extend(
        app.state::<crate::native_camera::CameraState>()
            .views
            .stream_sections(),
    );
    let (system, adapters, firewall, loopback) = tauri::async_runtime::spawn_blocking(|| {
        let mut system = Section::default();
        system.fact("os", std::env::consts::OS);
        system.fact("arch", std::env::consts::ARCH);
        match tauri::webview_version() {
            Ok(version) => system.fact("webview.version", version),
            Err(_) => system.fact("webview.version", "unavailable"),
        }
        let (adapters, firewall) = platform::collect(&mut system);
        (system, adapters, firewall, loopback())
    })
    .await
    .map_err(|_| "Video diagnostics collection failed")?;
    Ok(Report {
        app_version: env!("CARGO_PKG_VERSION").into(),
        system,
        adapters,
        firewall,
        loopback,
        streams,
        records,
    })
}

/// Repeats the bridge's packet path inside this process: a wildcard-bound
/// socket (as the WebView uses) exchanging UDP with a loopback-bound one.
fn loopback() -> Section {
    use std::{net::UdpSocket, time::Duration};
    fn round_trip(server: &str, client: &str) -> String {
        let result = (|| {
            let server = UdpSocket::bind(server)?;
            let client = UdpSocket::bind(client)?;
            for socket in [&server, &client] {
                socket.set_read_timeout(Some(Duration::from_millis(500)))?;
            }
            let started = std::time::Instant::now();
            client.send_to(b"thiscord-loopback-probe", server.local_addr()?)?;
            let mut buffer = [0; 64];
            let (_, source) = server.recv_from(&mut buffer)?;
            server.send_to(b"ok", source)?;
            client.recv_from(&mut buffer)?;
            Ok::<_, std::io::Error>(format!(
                "ok in {} µs, desktop saw {}",
                started.elapsed().as_micros(),
                thiscord_frontend::video_bridge::address_class(&source.ip().to_string())
            ))
        })();
        result.unwrap_or_else(|error| match error.raw_os_error() {
            Some(code) => format!("failed: {:?} (OS error {code})", error.kind()),
            None => format!("failed: {:?}", error.kind()),
        })
    }
    let mut section = Section::default();
    section.fact(
        "udp.wildcard_v4_to_loopback",
        round_trip("127.0.0.1:0", "0.0.0.0:0"),
    );
    section.fact("udp.loopback_v4", round_trip("127.0.0.1:0", "127.0.0.1:0"));
    section.fact(
        "udp.wildcard_v6_to_loopback",
        round_trip("[::1]:0", "[::]:0"),
    );
    section
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use thiscord_shared::video_bridge::Section;
    pub fn collect(_system: &mut Section) -> (Vec<Section>, Section) {
        let mut firewall = Section::default();
        firewall.fact(
            "status",
            "Adapter and firewall summaries are collected on Windows only.",
        );
        (vec![], firewall)
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::collections::BTreeMap;
    use thiscord_frontend::video_bridge::address_class;
    use thiscord_shared::video_bridge::Section;
    use windows::{
        Win32::{
            Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_MORE_ITEMS},
            NetworkManagement::{IpHelper::*, Ndis::IfOperStatusUp, WindowsFirewall::*},
            Networking::WinSock::*,
            System::{Com::*, Registry::*, Variant::VARIANT, Wmi::*},
        },
        core::*,
    };

    pub fn collect(system: &mut Section) -> (Vec<Section>, Section) {
        windows_version(system);
        webview_settings(system);
        let adapters = adapters();
        let mut firewall = Section::default();
        // COM is per thread; this blocking worker may already be initialized.
        let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        if let Err(error) = firewall_state(&mut firewall) {
            firewall.fact("error", format!("Firewall query failed: {}", error.code()));
        }
        if let Err(error) = security_products(&mut firewall) {
            firewall.fact(
                "security_products_error",
                format!("Security Center query failed: {}", error.code()),
            );
        }
        if initialized {
            unsafe { CoUninitialize() };
        }
        (adapters, firewall)
    }

    fn registry_string(root: HKEY, key: &str, value: &str) -> Option<String> {
        let (key, value) = (HSTRING::from(key), HSTRING::from(value));
        let mut size = 0u32;
        unsafe {
            RegGetValueW(
                root,
                &key,
                &value,
                RRF_RT_REG_SZ,
                None,
                None,
                Some(&mut size),
            )
        }
        .ok()
        .ok()?;
        let mut buffer = vec![0u16; size as usize / 2 + 1];
        unsafe {
            RegGetValueW(
                root,
                &key,
                &value,
                RRF_RT_REG_SZ,
                None,
                Some(buffer.as_mut_ptr().cast()),
                Some(&mut size),
            )
        }
        .ok()
        .ok()?;
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..end]))
    }
    fn registry_dword(root: HKEY, key: &str, value: &str) -> Option<u32> {
        let (key, value) = (HSTRING::from(key), HSTRING::from(value));
        let mut data = 0u32;
        let mut size = 4u32;
        unsafe {
            RegGetValueW(
                root,
                &key,
                &value,
                RRF_RT_REG_DWORD,
                None,
                Some((&mut data as *mut u32).cast()),
                Some(&mut size),
            )
        }
        .ok()
        .ok()?;
        Some(data)
    }
    /// Value names and short scalar data under a key, optionally filtered by name.
    fn registry_values(root: HKEY, key: &str, filter: &[&str]) -> Vec<(String, String)> {
        let mut handle = HKEY::default();
        let key = HSTRING::from(key);
        if unsafe { RegOpenKeyExW(root, &key, None, KEY_READ, &mut handle) }.is_err() {
            return vec![];
        }
        let mut values = Vec::new();
        for index in 0..64 {
            let mut name = [0u16; 256];
            let mut name_len = name.len() as u32;
            let mut kind = 0u32;
            let mut data = [0u8; 512];
            let mut data_len = data.len() as u32;
            let status = unsafe {
                RegEnumValueW(
                    handle,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut name_len,
                    None,
                    Some(&mut kind),
                    Some(data.as_mut_ptr()),
                    Some(&mut data_len),
                )
            };
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            if status.is_err() {
                continue;
            }
            let name = String::from_utf16_lossy(&name[..name_len as usize]);
            let lower = name.to_ascii_lowercase();
            if !filter.is_empty() && !filter.iter().any(|f| lower.contains(f)) {
                continue;
            }
            let value = match REG_VALUE_TYPE(kind) {
                REG_DWORD if data_len >= 4 => {
                    u32::from_le_bytes([data[0], data[1], data[2], data[3]]).to_string()
                }
                REG_SZ | REG_EXPAND_SZ => {
                    let wide: Vec<u16> = data[..data_len as usize]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|&c| u16::from_le_bytes(c))
                        .take_while(|&c| c != 0)
                        .collect();
                    String::from_utf16_lossy(&wide)
                }
                _ => format!("type {kind}"),
            };
            values.push((name, value));
        }
        unsafe {
            let _ = RegCloseKey(handle);
        }
        values
    }
    fn subkeys(root: HKEY, key: &str) -> Vec<String> {
        let mut handle = HKEY::default();
        if unsafe { RegOpenKeyExW(root, &HSTRING::from(key), None, KEY_READ, &mut handle) }.is_err()
        {
            return vec![];
        }
        let mut names = Vec::new();
        for index in 0..64 {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            let status = unsafe {
                RegEnumKeyExW(
                    handle,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if status.is_err() {
                break;
            }
            names.push(String::from_utf16_lossy(&name[..len as usize]));
        }
        unsafe {
            let _ = RegCloseKey(handle);
        }
        names
    }

    fn windows_version(system: &mut Section) {
        const KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
        for (fact, value) in [
            ("windows.product", "ProductName"),
            ("windows.display_version", "DisplayVersion"),
            ("windows.release_id", "ReleaseId"),
            ("windows.edition", "EditionID"),
            ("windows.installation", "InstallationType"),
        ] {
            if let Some(text) = registry_string(HKEY_LOCAL_MACHINE, KEY, value) {
                system.fact(fact, text);
            }
        }
        let build = registry_string(HKEY_LOCAL_MACHINE, KEY, "CurrentBuild")
            .and_then(|b| b.parse::<u32>().ok());
        if let Some(build) = build {
            let revision = registry_dword(HKEY_LOCAL_MACHINE, KEY, "UBR").unwrap_or(0);
            system.fact("windows.build", format!("{build}.{revision}"));
            // ProductName still says "Windows 10" on Windows 11.
            system.fact(
                "windows.family",
                if build >= 22000 {
                    "Windows 11"
                } else {
                    "Windows 10 or older"
                },
            );
        }
    }

    fn webview_settings(system: &mut Section) {
        const EDGE: &str = r"SOFTWARE\Policies\Microsoft\Edge";
        let mut policies = Vec::new();
        for (hive, root) in [("HKLM", HKEY_LOCAL_MACHINE), ("HKCU", HKEY_CURRENT_USER)] {
            for (name, value) in registry_values(root, EDGE, &["webrtc", "proxy"]) {
                policies.push(format!("{hive} Edge\\{name}={value}"));
            }
            let webview = format!(r"{EDGE}\WebView2");
            for policy in subkeys(root, &webview) {
                for (app, value) in registry_values(root, &format!(r"{webview}\{policy}"), &[]) {
                    policies.push(format!("{hive} WebView2\\{policy}[{app}]={value}"));
                }
            }
        }
        if !policies.is_empty() {
            system.fact("webview2.policies", policies.join("; "));
        }
        if let Ok(arguments) = std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS") {
            system.fact("webview2.env_arguments", arguments);
        }
        for name in [
            "WEBVIEW2_BROWSER_EXECUTABLE_FOLDER",
            "WEBVIEW2_RELEASE_CHANNEL_PREFERENCE",
            "WEBVIEW2_USER_DATA_FOLDER",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
        ] {
            if std::env::var_os(name).is_some() {
                system.fact(format!("env.{}", name.to_ascii_lowercase()), "set");
            }
        }
        const INTERNET: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
        if let Some(enabled) = registry_dword(HKEY_CURRENT_USER, INTERNET, "ProxyEnable") {
            system.fact("proxy.enabled", enabled != 0);
        }
        for (fact, value) in [
            ("proxy.server", "ProxyServer"),
            ("proxy.auto_config", "AutoConfigURL"),
        ] {
            if registry_string(HKEY_CURRENT_USER, INTERNET, value).is_some_and(|v| !v.is_empty()) {
                system.fact(fact, "configured");
            }
        }
    }

    fn adapters() -> Vec<Section> {
        let flags = GAA_FLAG_INCLUDE_GATEWAYS
            | GAA_FLAG_SKIP_DNS_SERVER
            | GAA_FLAG_SKIP_MULTICAST
            | GAA_FLAG_SKIP_ANYCAST;
        let mut size = 32 * 1024u32;
        let mut buffer = Vec::<u64>::new();
        for _ in 0..3 {
            buffer = vec![0u64; size as usize / 8 + 1];
            let status = unsafe {
                GetAdaptersAddresses(
                    AF_UNSPEC.0 as u32,
                    flags,
                    None,
                    Some(buffer.as_mut_ptr().cast()),
                    &mut size,
                )
            };
            if status == ERROR_BUFFER_OVERFLOW.0 {
                continue;
            }
            if status != 0 {
                return vec![];
            }
            break;
        }
        let mut sections = Vec::new();
        let mut adapter = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !adapter.is_null() && sections.len() < 24 {
            let a = unsafe { &*adapter };
            let mut section = Section::default();
            let text = |p: PWSTR| unsafe { p.to_string() }.unwrap_or_default();
            let description = text(a.Description);
            section.fact("name", text(a.FriendlyName));
            section.fact("description", &description);
            section.fact(
                "type",
                match a.IfType {
                    6 => "ethernet".to_owned(),
                    71 => "wifi".to_owned(),
                    24 => "loopback".to_owned(),
                    23 => "ppp".to_owned(),
                    131 => "tunnel".to_owned(),
                    53 => "virtual".to_owned(),
                    other => format!("iftype {other}"),
                },
            );
            section.fact(
                "status",
                if a.OperStatus == IfOperStatusUp {
                    "up"
                } else {
                    "down"
                },
            );
            let lower = description.to_ascii_lowercase();
            let vpn_like = matches!(a.IfType, 23 | 53 | 131)
                || [
                    "vpn",
                    "tap-",
                    "wintun",
                    "wireguard",
                    "warp",
                    "radmin",
                    "hamachi",
                    "zerotier",
                    "tailscale",
                    "openvpn",
                    "anyconnect",
                    "fortinet",
                    "nord",
                    "proton",
                ]
                .iter()
                .any(|k| lower.contains(k));
            section.fact("vpn_like", vpn_like);
            let mut classes = BTreeMap::<&str, usize>::new();
            let mut unicast = a.FirstUnicastAddress;
            while !unicast.is_null() {
                let u = unsafe { &*unicast };
                if let Some(ip) = socket_ip(u.Address.lpSockaddr) {
                    *classes.entry(address_class(&ip.to_string())).or_default() += 1;
                }
                unicast = u.Next;
            }
            section.fact(
                "addresses",
                classes
                    .iter()
                    .map(|(c, n)| format!("{c}×{n}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            let mut gateways = Vec::new();
            let mut gateway = a.FirstGatewayAddress;
            while !gateway.is_null() {
                let g = unsafe { &*gateway };
                match socket_ip(g.Address.lpSockaddr) {
                    Some(std::net::IpAddr::V4(_)) => gateways.push("v4"),
                    Some(std::net::IpAddr::V6(_)) => gateways.push("v6"),
                    None => {}
                }
                gateway = g.Next;
            }
            gateways.dedup();
            section.fact(
                "gateway",
                if gateways.is_empty() {
                    "none".into()
                } else {
                    gateways.join("+")
                },
            );
            section.fact(
                "metric",
                format!("v4 {} / v6 {}", a.Ipv4Metric, a.Ipv6Metric),
            );
            section.fact("mtu", a.Mtu);
            sections.push(section);
            adapter = a.Next;
        }
        sections
    }
    fn socket_ip(address: *const SOCKADDR) -> Option<std::net::IpAddr> {
        if address.is_null() {
            return None;
        }
        unsafe {
            match (*address).sa_family {
                AF_INET => {
                    let v4 = &*(address as *const SOCKADDR_IN);
                    Some(std::net::Ipv4Addr::from(u32::from_be(v4.sin_addr.S_un.S_addr)).into())
                }
                AF_INET6 => {
                    let v6 = &*(address as *const SOCKADDR_IN6);
                    Some(std::net::Ipv6Addr::from(v6.sin6_addr.u.Byte).into())
                }
                _ => None,
            }
        }
    }

    fn firewall_state(section: &mut Section) -> Result<()> {
        let policy: INetFwPolicy2 =
            unsafe { CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER) }?;
        let current = unsafe { policy.CurrentProfileTypes() }?;
        for (name, profile) in [
            ("domain", NET_FW_PROFILE2_DOMAIN),
            ("private", NET_FW_PROFILE2_PRIVATE),
            ("public", NET_FW_PROFILE2_PUBLIC),
        ] {
            let enabled = unsafe { policy.get_FirewallEnabled(profile) }?.as_bool();
            let block_all = unsafe { policy.get_BlockAllInboundTraffic(profile) }?.as_bool();
            let inbound = unsafe { policy.get_DefaultInboundAction(profile) }?;
            section.fact(
                format!("profile.{name}"),
                format!(
                    "{}enabled={enabled}, block_all_inbound={block_all}, default_inbound={}",
                    if current & profile.0 != 0 {
                        "active, "
                    } else {
                        ""
                    },
                    if inbound == NET_FW_ACTION_BLOCK {
                        "block"
                    } else {
                        "allow"
                    }
                ),
            );
        }
        let executable = std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().to_ascii_lowercase());
        let rules = unsafe { policy.Rules() }?;
        section.fact("rules.total", unsafe { rules.Count() }?);
        let enumerator: windows::Win32::System::Ole::IEnumVARIANT =
            unsafe { rules._NewEnum() }?.cast()?;
        let (mut block, mut allow) = (Vec::new(), Vec::new());
        loop {
            let mut items = [VARIANT::default()];
            let mut fetched = 0u32;
            if unsafe { enumerator.Next(&mut items, &mut fetched) }.is_err() || fetched == 0 {
                break;
            }
            let Ok(dispatch) = IDispatch::try_from(&items[0]) else {
                continue;
            };
            let Ok(rule) = dispatch.cast::<INetFwRule>() else {
                continue;
            };
            let application = unsafe { rule.ApplicationName() }
                .map(|a| a.to_string().to_ascii_lowercase())
                .unwrap_or_default();
            let name = unsafe { rule.Name() }
                .map(|n| n.to_string())
                .unwrap_or_default();
            let relevant = application.contains("msedgewebview2")
                || application.contains("thiscord")
                || executable
                    .as_ref()
                    .is_some_and(|e| !application.is_empty() && *e == application)
                || name.to_ascii_lowercase().contains("thiscord");
            if !relevant {
                continue;
            }
            let enabled = unsafe { rule.Enabled() }.is_ok_and(|e| e.as_bool());
            let inbound = unsafe { rule.Direction() }.is_ok_and(|d| d == NET_FW_RULE_DIR_IN);
            let protocol = match unsafe { rule.Protocol() }.unwrap_or(256) {
                6 => "tcp".to_owned(),
                17 => "udp".to_owned(),
                256 => "any".to_owned(),
                other => format!("proto {other}"),
            };
            let profiles = unsafe { rule.Profiles() }.unwrap_or(0);
            let profiles = [(1, "domain"), (2, "private"), (4, "public")]
                .iter()
                .filter(|(bit, _)| profiles & bit != 0)
                .map(|(_, n)| *n)
                .collect::<Vec<_>>()
                .join("+");
            let program = if application.contains("msedgewebview2") {
                "msedgewebview2"
            } else if application.contains("thiscord")
                || name.to_ascii_lowercase().contains("thiscord")
            {
                "thiscord"
            } else {
                "this app"
            };
            let description = format!(
                "{name} [{program}, {}, {protocol}, {profiles}{}]",
                if inbound { "in" } else { "out" },
                if enabled { "" } else { ", disabled" }
            );
            let list = if unsafe { rule.Action() }.is_ok_and(|a| a == NET_FW_ACTION_BLOCK) {
                &mut block
            } else {
                &mut allow
            };
            if list.len() < 12 {
                list.push(description);
            }
        }
        for (key, list) in [("rules.block", block), ("rules.allow", allow)] {
            for (i, rule) in list.into_iter().enumerate() {
                section.fact(format!("{key}.{i}"), rule);
            }
        }
        Ok(())
    }

    /// Antivirus/firewall products registered with Windows Security Center.
    fn security_products(section: &mut Section) -> Result<()> {
        let locator: IWbemLocator =
            unsafe { CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER) }?;
        let services = unsafe {
            locator.ConnectServer(
                &BSTR::from(r"ROOT\SecurityCenter2"),
                &BSTR::new(),
                &BSTR::new(),
                &BSTR::new(),
                0,
                &BSTR::new(),
                None,
            )
        }?;
        let mut products = Vec::new();
        for class in ["AntiVirusProduct", "FirewallProduct"] {
            let query = unsafe {
                services.ExecQuery(
                    &BSTR::from("WQL"),
                    &BSTR::from(format!("SELECT displayName, productState FROM {class}")),
                    WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                    None,
                )
            }?;
            for _ in 0..16 {
                let mut row = [None];
                let mut returned = 0u32;
                if unsafe { query.Next(2000, &mut row, &mut returned) }.is_err() || returned == 0 {
                    break;
                }
                let Some(object) = row[0].take() else { break };
                let mut name = VARIANT::default();
                let mut state = VARIANT::default();
                unsafe {
                    object.Get(w!("displayName"), 0, &mut name, None, None)?;
                    object.Get(w!("productState"), 0, &mut state, None, None)?;
                }
                let name = BSTR::try_from(&name)
                    .map(|b| b.to_string())
                    .unwrap_or_default();
                let state = i32::try_from(&state).unwrap_or(0);
                // Bits 12-13 report whether the product is turned on.
                let active = (state >> 12) & 0x3 == 1;
                products.push(format!(
                    "{name} ({}, {})",
                    if class == "FirewallProduct" {
                        "firewall"
                    } else {
                        "antivirus"
                    },
                    if active { "on" } else { "off" }
                ));
            }
        }
        section.fact(
            "security_products",
            if products.is_empty() {
                "none registered (Windows Defender only, or Windows Server)".into()
            } else {
                products.join("; ")
            },
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_report_classifies_without_exposing_addresses() {
        let section = loopback();
        assert!(
            section.facts["udp.loopback_v4"].starts_with("ok"),
            "{section:?}"
        );
        let mut system = Section::default();
        let (adapters, firewall) = platform::collect(&mut system);
        let text = serde_json::to_string(&(system, adapters, firewall, section)).unwrap();
        assert!(
            !text.contains("127.0.0.1") && !text.contains("::1"),
            "{text}"
        );
        let raw_ipv4 = text
            .split(|c: char| !c.is_ascii_digit() && c != '.')
            .any(|token| {
                token.split('.').count() == 4 && token.split('.').all(|p| p.parse::<u8>().is_ok())
            });
        assert!(!raw_ipv4, "{text}");
        if std::env::var_os("THISCORD_PRINT_VIDEO_DIAGNOSTICS").is_some() {
            println!("{text}");
        }
    }
}
