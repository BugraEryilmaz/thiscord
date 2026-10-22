//! Public GitHub release metadata. No account/session credentials are involved.
use serde::Deserialize;

pub const REPOSITORY: &str = "https://github.com/BugraEryilmaz/thiscord";
pub const LATEST_PAGE: &str = "https://github.com/BugraEryilmaz/thiscord/releases/latest";
pub const LATEST_API: &str = "https://api.github.com/repos/BugraEryilmaz/thiscord/releases/latest";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Mac,
    Linux,
    Debian,
}

impl Platform {
    pub fn name(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::Mac => "macOS",
            Self::Linux => "Linux",
            Self::Debian => "Debian / Ubuntu",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Windows => "64-bit installer (.exe)",
            Self::Mac => "Apple Silicon (.dmg)",
            Self::Linux => "64-bit AppImage",
            Self::Debian => "64-bit package (.deb)",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Windows => "windows-x86_64.exe",
            Self::Mac => "darwin-aarch64.dmg",
            Self::Linux => "linux-x86_64.AppImage",
            Self::Debian => "linux-x86_64.deb",
        }
    }

    // A convenience suggestion only; all architectures remain explicitly labeled.
    // Mac UAs cannot reliably distinguish Intel from Apple Silicon.
    pub fn suggested(user_agent: &str) -> Option<Self> {
        if ["Android", "iPhone", "iPad", "Mobile"]
            .iter()
            .any(|s| user_agent.contains(s))
        {
            None
        } else if user_agent.contains("Windows") {
            Some(Self::Windows)
        } else if user_agent.contains("Macintosh") {
            Some(Self::Mac)
        } else if user_agent.contains("Linux") && user_agent.contains("x86_64") {
            Some(Self::Linux)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub draft: bool,
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    pub state: String,
    pub size: u64,
}

impl Release {
    pub fn version(&self) -> Option<&str> {
        if self.draft || self.prerelease {
            return None;
        }
        let version = self.tag_name.strip_prefix("client-v")?;
        let parts: Vec<_> = version.split('.').collect();
        (parts.len() == 3
            && parts.iter().all(|part| {
                !part.is_empty()
                    && part.bytes().all(|b| b.is_ascii_digit())
                    && (part.len() == 1 || !part.starts_with('0'))
            }))
        .then_some(version)
    }

    pub fn download(&self, platform: Platform) -> Option<&Asset> {
        let name = format!("Thiscord-{}-{}", self.version()?, platform.suffix());
        let expected = format!("{REPOSITORY}/releases/download/{}/{name}", self.tag_name);
        self.assets.iter().find(|asset| {
            asset.name == name
                && asset.browser_download_url == expected
                && asset.state == "uploaded"
                && asset.size > 0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release() -> Release {
        let mut release = Release {
            tag_name: "client-v1.2.3".into(),
            draft: false,
            prerelease: false,
            assets: vec![],
        };
        for platform in [
            Platform::Windows,
            Platform::Mac,
            Platform::Linux,
            Platform::Debian,
        ] {
            let name = format!("Thiscord-1.2.3-{}", platform.suffix());
            release.assets.push(Asset {
                browser_download_url: format!(
                    "{REPOSITORY}/releases/download/client-v1.2.3/{name}"
                ),
                name,
                state: "uploaded".into(),
                size: 123,
            });
        }
        release
    }

    #[test]
    fn selects_installers_for_each_platform_not_update_archives() {
        let mut release = release();
        release.assets.insert(
            0,
            Asset {
                name: "Thiscord-1.2.3-darwin-aarch64.app.tar.gz".into(),
                browser_download_url: "unused".into(),
                state: "uploaded".into(),
                size: 42,
            },
        );
        for platform in [
            Platform::Windows,
            Platform::Mac,
            Platform::Linux,
            Platform::Debian,
        ] {
            assert!(
                release
                    .download(platform)
                    .unwrap()
                    .name
                    .ends_with(platform.suffix())
            );
        }
    }

    #[test]
    fn rejects_unpublished_and_non_client_versions() {
        for tag in [
            "v1.2.3",
            "client-v1.2",
            "client-v1.2.3-beta",
            "client-v1.2.03",
            "client-v../1.2.3",
        ] {
            let mut release = release();
            release.tag_name = tag.into();
            assert_eq!(release.version(), None);
        }
        let mut release = release();
        release.draft = true;
        assert!(release.download(Platform::Windows).is_none());
        release.draft = false;
        release.prerelease = true;
        assert!(release.download(Platform::Windows).is_none());
    }

    #[test]
    fn rejects_missing_empty_foreign_or_mismatched_assets() {
        for url in [
            "https://example.com/installer.exe",
            "javascript:alert(1)",
            "https://github.com/BugraEryilmaz/thiscord/releases/download/client-v1.0.0/Thiscord-1.2.3-windows-x86_64.exe",
        ] {
            let mut release = release();
            release.assets[0].browser_download_url = url.into();
            assert!(release.download(Platform::Windows).is_none());
        }
        let mut release = release();
        release.assets[0].size = 0;
        assert!(release.download(Platform::Windows).is_none());
        release.assets.clear();
        assert!(release.download(Platform::Mac).is_none());
    }

    #[test]
    fn mobile_and_unknown_devices_get_platform_choices() {
        for ua in [
            "iPhone",
            "Linux Android",
            "Macintosh Mobile",
            "Linux aarch64",
            "",
        ] {
            assert_eq!(Platform::suggested(ua), None);
        }
        assert_eq!(
            Platform::suggested("Windows NT 10.0; Win64; x64"),
            Some(Platform::Windows)
        );
        assert_eq!(
            Platform::suggested("Macintosh; Intel Mac OS X"),
            Some(Platform::Mac)
        );
        assert_eq!(
            Platform::suggested("X11; Linux x86_64"),
            Some(Platform::Linux)
        );
    }
}
