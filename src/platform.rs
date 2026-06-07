//! Target platform resolution.
//!
//! The platform is *supplied* via `--platform`, never detected from the host,
//! so a build can target any platform from anywhere. One token
//! expands into a [`Ctx`] carrying both rule dialects' view of the platform:
//! the MMC arch-in-name token (`osx-arm64`) and the classic Mojang
//! `name`/`arch`/`version`, plus the classpath separator and feature flags.

use std::collections::HashMap;

use clap::ValueEnum;

/// The fixed set of acceptable `--platform` tokens. Anything
/// outside this list is rejected by clap before the pipeline runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Platform {
    #[value(name = "linux")]
    Linux,
    #[value(name = "linux-arm64")]
    LinuxArm64,
    #[value(name = "linux-arm32")]
    LinuxArm32,
    #[value(name = "linux-ppc64le")]
    LinuxPpc64le,
    #[value(name = "freebsd")]
    FreeBsd,
    #[value(name = "osx")]
    Osx,
    #[value(name = "osx-arm64")]
    OsxArm64,
    #[value(name = "windows")]
    Windows,
    #[value(name = "windows-arm64")]
    WindowsArm64,
    #[value(name = "windows-x86")]
    WindowsX86,
}

/// The evaluation context for `allowed()` - one platform seen through both rule
/// dialects, with the classpath separator the emitter must use.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// MMC arch-in-name token, e.g. `linux`, `osx-arm64`, `windows-x86`.
    pub os_token: String,
    /// Classic Mojang OS name: `linux` / `osx` / `windows` / `freebsd`.
    pub os_name: String,
    /// Classic arch: `x86` / `x86_64` / `arm64` / `arm32` / `ppc64le`.
    pub arch: String,
    /// OS version string for classic `os.version` regex rules.
    pub version: String,
    /// Feature flags gating modern game args; absent keys default false.
    pub features: HashMap<String, bool>,
    /// Classpath separator for the *target*: `;` on windows, else `:`. Used by
    /// the assembler; set here so the decision is made once.
    pub path_sep: char,
}

impl Ctx {
    /// The `${arch}` marker substituted into legacy native classifier templates
    /// (e.g. `natives-windows-${arch}` -> `...-64`); `"32"` or `"64"` by bitness.
    #[must_use]
    pub fn arch_number(&self) -> &'static str {
        match self.arch.as_str() {
            "x86" | "arm32" => "32",
            _ => "64",
        }
    }

    /// Look up a feature flag, defaulting to `false`.
    #[must_use]
    pub fn feature(&self, name: &str) -> bool {
        self.features.get(name).copied().unwrap_or(false)
    }
}

/// Expand a validated platform token into its evaluation [`Ctx`].
///
/// Note `linux` maps to x86-64 specifically. The `version`
/// values are sensible modern defaults - classic `os.version` rules rarely
/// appear in Prism patches, and when they do they gate *old* OSes out.
#[must_use]
pub fn expand_platform(platform: Platform) -> Ctx {
    // (token, classic os name, classic arch, default os version)
    let (token, os_name, arch, version) = match platform {
        Platform::Linux => ("linux", "linux", "x86_64", "6.1.0"),
        Platform::LinuxArm64 => ("linux-arm64", "linux", "arm64", "6.1.0"),
        Platform::LinuxArm32 => ("linux-arm32", "linux", "arm32", "6.1.0"),
        Platform::LinuxPpc64le => ("linux-ppc64le", "linux", "ppc64le", "6.1.0"),
        Platform::FreeBsd => ("freebsd", "freebsd", "x86_64", "14.0"),
        Platform::Osx => ("osx", "osx", "x86_64", "13.0.0"),
        Platform::OsxArm64 => ("osx-arm64", "osx", "arm64", "13.0.0"),
        Platform::Windows => ("windows", "windows", "x86_64", "10.0"),
        Platform::WindowsArm64 => ("windows-arm64", "windows", "arm64", "10.0"),
        Platform::WindowsX86 => ("windows-x86", "windows", "x86", "10.0"),
    };

    // Separator follows the target OS, never the host.
    let path_sep = if os_name == "windows" { ';' } else { ':' };

    Ctx {
        os_token: token.to_owned(),
        os_name: os_name.to_owned(),
        arch: arch.to_owned(),
        version: version.to_owned(),
        features: HashMap::new(),
        path_sep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_token_is_x86_64_with_colon_separator() {
        let ctx = expand_platform(Platform::Linux);
        assert_eq!(ctx.os_token, "linux");
        assert_eq!(ctx.os_name, "linux");
        assert_eq!(ctx.arch, "x86_64");
        assert_eq!(ctx.path_sep, ':');
        assert_eq!(ctx.arch_number(), "64");
    }

    #[test]
    fn windows_tokens_use_semicolon_separator() {
        assert_eq!(expand_platform(Platform::Windows).path_sep, ';');
        assert_eq!(expand_platform(Platform::WindowsArm64).path_sep, ';');
        assert_eq!(expand_platform(Platform::WindowsX86).path_sep, ';');
    }

    #[test]
    fn windows_x86_maps_to_x86_and_arch_marker_32() {
        let ctx = expand_platform(Platform::WindowsX86);
        assert_eq!(ctx.os_name, "windows");
        assert_eq!(ctx.arch, "x86");
        assert_eq!(ctx.arch_number(), "32");
    }

    #[test]
    fn osx_arm64_splits_token_into_name_and_arch() {
        let ctx = expand_platform(Platform::OsxArm64);
        assert_eq!(ctx.os_token, "osx-arm64");
        assert_eq!(ctx.os_name, "osx");
        assert_eq!(ctx.arch, "arm64");
        assert_eq!(ctx.path_sep, ':');
    }

    #[test]
    fn features_default_to_false() {
        let ctx = expand_platform(Platform::Linux);
        assert!(!ctx.feature("is_demo_user"));
        assert!(!ctx.feature("has_custom_resolution"));
    }
}
