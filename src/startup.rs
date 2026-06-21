use std::path::{Path, PathBuf};

use crate::error::AppError;

pub const HIDDEN_ARG: &str = "--hidden";

#[cfg(any(target_os = "windows", target_os = "macos"))]
const APP_NAME: &str = "ResearchWiki";
#[cfg(any(target_os = "windows", target_os = "macos"))]
const BUNDLE_IDENTIFIER: &str = "com.researchwiki.app";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoginStartupStatus {
    pub supported: bool,
    pub enabled: bool,
}

impl LoginStartupStatus {
    pub const fn unsupported() -> Self {
        Self {
            supported: false,
            enabled: false,
        }
    }
}

pub fn is_hidden_launch<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().any(|arg| arg.as_ref() == HIDDEN_ARG)
}

pub fn login_startup_status() -> Result<LoginStartupStatus, AppError> {
    platform::login_startup_status()
}

pub fn set_login_startup_enabled(enabled: bool) -> Result<LoginStartupStatus, AppError> {
    platform::set_login_startup_enabled(enabled)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn current_login_item_app_path() -> Result<PathBuf, AppError> {
    let exe = std::env::current_exe()?;
    Ok(login_item_app_path_from_exe(&exe))
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn login_item_app_path_from_exe(exe: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(bundle) = macos_bundle_path_from_exe(exe) {
            return bundle;
        }
    }
    exe.to_path_buf()
}

pub fn macos_bundle_path_from_exe(exe: &Path) -> Option<PathBuf> {
    let macos_dir = exe.parent()?;
    if macos_dir.file_name()? != "MacOS" {
        return None;
    }
    let contents_dir = macos_dir.parent()?;
    if contents_dir.file_name()? != "Contents" {
        return None;
    }
    let app_dir = contents_dir.parent()?;
    (app_dir.extension()? == "app").then(|| app_dir.to_path_buf())
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod platform {
    use auto_launch::AutoLaunchBuilder;

    use super::{
        APP_NAME, AppError, BUNDLE_IDENTIFIER, HIDDEN_ARG, LoginStartupStatus,
        current_login_item_app_path,
    };

    pub fn login_startup_status() -> Result<LoginStartupStatus, AppError> {
        let auto = auto_launch()?;
        let enabled = auto
            .is_enabled()
            .map_err(|error| AppError::Internal(error.to_string()))?;
        Ok(LoginStartupStatus {
            supported: true,
            enabled,
        })
    }

    pub fn set_login_startup_enabled(enabled: bool) -> Result<LoginStartupStatus, AppError> {
        let auto = auto_launch()?;
        let result = if enabled {
            auto.enable()
        } else {
            auto.disable()
        };
        result.map_err(|error| AppError::Internal(error.to_string()))?;
        login_startup_status()
    }

    fn auto_launch() -> Result<auto_launch::AutoLaunch, AppError> {
        let app_path = current_login_item_app_path()?;
        let app_path = app_path.to_string_lossy();
        let mut builder = AutoLaunchBuilder::new();
        builder
            .set_app_name(APP_NAME)
            .set_app_path(&app_path)
            .set_args(&[HIDDEN_ARG]);

        #[cfg(target_os = "windows")]
        {
            builder.set_windows_enable_mode(auto_launch::WindowsEnableMode::CurrentUser);
        }
        #[cfg(target_os = "macos")]
        {
            builder
                .set_macos_launch_mode(auto_launch::MacOSLaunchMode::LaunchAgent)
                .set_bundle_identifiers(&[BUNDLE_IDENTIFIER]);
        }

        builder
            .build()
            .map_err(|error| AppError::Internal(error.to_string()))
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod platform {
    use super::{AppError, LoginStartupStatus};

    pub fn login_startup_status() -> Result<LoginStartupStatus, AppError> {
        Ok(LoginStartupStatus::unsupported())
    }

    pub fn set_login_startup_enabled(enabled: bool) -> Result<LoginStartupStatus, AppError> {
        if enabled {
            return Err(AppError::BadRequest(
                "Login startup is only available in macOS and Windows desktop builds.".to_string(),
            ));
        }
        Ok(LoginStartupStatus::unsupported())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        HIDDEN_ARG, LoginStartupStatus, is_hidden_launch, macos_bundle_path_from_exe,
        set_login_startup_enabled,
    };

    #[test]
    fn hidden_launch_detects_exact_flag() {
        assert!(is_hidden_launch(["researchwiki", HIDDEN_ARG]));
        assert!(!is_hidden_launch(["researchwiki", "--hidden=false"]));
    }

    #[test]
    fn macos_bundle_path_resolves_from_bundle_executable() {
        let exe = Path::new("/Applications/ResearchWiki.app/Contents/MacOS/researchwiki");
        assert_eq!(
            macos_bundle_path_from_exe(exe),
            Some(PathBuf::from("/Applications/ResearchWiki.app"))
        );
    }

    #[test]
    fn macos_bundle_path_ignores_unbundled_executable() {
        assert_eq!(
            macos_bundle_path_from_exe(Path::new("/usr/bin/researchwiki")),
            None
        );
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    #[test]
    fn unsupported_platform_reports_disabled() {
        assert_eq!(
            super::login_startup_status().unwrap(),
            LoginStartupStatus::unsupported()
        );
        assert!(set_login_startup_enabled(false).is_ok());
        assert!(set_login_startup_enabled(true).is_err());
    }
}
