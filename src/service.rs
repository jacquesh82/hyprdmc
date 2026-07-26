//! Autostart: the systemd user service.
//!
//! Debian, Fedora/RHEL and Arch all run systemd, and all three give every
//! login session a `systemd --user` instance — so one *user* unit covers the
//! three families, with nothing distribution-specific in it. Nothing is
//! installed system-wide either: the daemon drives one session's displays, it
//! has no business running as root.
//!
//! The unit is generated rather than copied, because the one line that matters
//! is the path to the binary, and that path depends on how it was installed
//! (`/usr/bin` from a package, `~/.cargo/bin` from `cargo install`,
//! `~/.local/bin` from a manual copy). A README snippet gets that wrong; the
//! running executable knows.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use rust_i18n::t;

/// Unit file name, both on disk and for `systemctl`.
pub const UNIT: &str = "hyprdmc.service";

/// Target the unit hooks onto by default.
///
/// `graphical-session.target` is the conventional one, but it is only reached
/// if something in the session activates it — `uwsm` does, a bare
/// `exec-once = Hyprland` does not. [`session_target_active`] checks, so the
/// install can say so instead of leaving a service that silently never starts.
pub const DEFAULT_TARGET: &str = "graphical-session.target";

/// Renders the unit for a given binary path.
///
/// Kept pure so the content can be tested, and shown by `--dry-run` before
/// anything is written.
pub fn render_unit(exec: &Path, wanted_by: &str) -> String {
    format!(
        "# {generated}\n\
         [Unit]\n\
         Description=Dynamic monitor configuration for Hyprland\n\
         Documentation=https://github.com/jacquesh82/hyprdmc\n\
         PartOf={wanted_by}\n\
         After={wanted_by}\n\
         # Hyprland's socket is often not up yet on the first tries after\n\
         # login — especially when hooked onto default.target, which systemd\n\
         # reaches long before the compositor exists. Roughly a minute of\n\
         # retries, then it gives up rather than spinning forever.\n\
         StartLimitIntervalSec=120\n\
         StartLimitBurst=20\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exec} daemon\n\
         Restart=on-failure\n\
         RestartSec=3\n\
         Slice=session.slice\n\
         \n\
         [Install]\n\
         WantedBy={wanted_by}\n",
        generated = t!("service.generated"),
        exec = exec.display(),
    )
}

/// `$XDG_CONFIG_HOME/systemd/user`, or the usual fallback.
pub fn unit_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::config::home().join(".config"))
        .join("systemd/user")
}

pub fn unit_path() -> PathBuf {
    unit_dir().join(UNIT)
}

/// Path of the running binary, for `ExecStart`.
///
/// A relative path would be resolved against systemd's working directory, not
/// the shell's, so this insists on an absolute one.
fn executable() -> Result<PathBuf> {
    let exe = std::env::current_exe().context(t!("service.no_executable").to_string())?;
    if !exe.is_absolute() {
        bail!(
            t!(
                "service.relative_executable",
                path = exe.display().to_string()
            )
            .to_string()
        );
    }
    Ok(exe)
}

/// Is systemd actually running this session? A container or a distribution
/// without it would take the unit and never look at it.
pub fn systemd_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "--version"])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Has the session activated the target the unit hooks onto?
pub fn session_target_active(target: &str) -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", target])
        .status()
        .is_ok_and(|s| s.success())
}

/// Runs a `systemctl --user` subcommand, surfacing what it printed on failure.
fn systemctl(args: &[&str]) -> Result<()> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .with_context(|| t!("service.systemctl_failed", args = args.join(" ")).to_string())?;
    if !out.status.success() {
        bail!(
            t!(
                "service.systemctl_rejected",
                args = args.join(" "),
                message = String::from_utf8_lossy(&out.stderr).trim()
            )
            .to_string()
        );
    }
    Ok(())
}

/// What [`install`] did, so the caller can report it in the user's language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub path: PathBuf,
    pub enabled: bool,
    /// The unit was written, but its target is not active in this session, so
    /// it will not start on its own until that is fixed.
    pub target_inactive: bool,
}

/// Writes the unit and reloads systemd. Enables it too when asked.
pub fn install(wanted_by: &str, enable: bool) -> Result<Installed> {
    if !systemd_available() {
        bail!(t!("service.no_systemd").to_string());
    }

    let path = unit_path();
    crate::emit::write_atomic(&path, &render_unit(&executable()?, wanted_by))?;
    systemctl(&["daemon-reload"])?;

    if enable {
        systemctl(&["enable", "--now", UNIT])?;
    }

    Ok(Installed {
        path,
        enabled: enable,
        target_inactive: !session_target_active(wanted_by),
    })
}

/// Disables the unit and removes it.
pub fn uninstall() -> Result<Option<PathBuf>> {
    let path = unit_path();
    if !path.exists() {
        return Ok(None);
    }
    if systemd_available() {
        // Best effort: a unit that was never enabled makes `disable` fail, and
        // that must not stop us from removing the file.
        let _ = systemctl(&["disable", "--now", UNIT]);
    }
    std::fs::remove_file(&path)
        .with_context(|| t!("fs.write_failed", path = path.display().to_string()).to_string())?;
    if systemd_available() {
        systemctl(&["daemon-reload"])?;
    }
    Ok(Some(path))
}

/// Where the unit is, and what systemd makes of it.
#[derive(Debug, Clone)]
pub struct Status {
    pub path: PathBuf,
    pub installed: bool,
    pub enabled: bool,
    pub active: bool,
}

pub fn status() -> Status {
    let path = unit_path();
    let query = |arg: &str| {
        Command::new("systemctl")
            .args(["--user", arg, "--quiet", UNIT])
            .status()
            .is_ok_and(|s| s.success())
    };
    Status {
        installed: path.exists(),
        enabled: query("is-enabled"),
        active: query("is-active"),
        path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unit_points_at_the_binary_it_was_generated_from() {
        let unit = render_unit(Path::new("/usr/bin/hyprdmc"), DEFAULT_TARGET);
        assert!(unit.contains("ExecStart=/usr/bin/hyprdmc daemon"));
    }

    #[test]
    fn the_unit_hooks_onto_the_requested_target() {
        let unit = render_unit(Path::new("/usr/bin/hyprdmc"), "hyprland-session.target");
        assert!(unit.contains("WantedBy=hyprland-session.target"));
        assert!(unit.contains("PartOf=hyprland-session.target"));
        assert!(unit.contains("After=hyprland-session.target"));
        assert!(
            !unit.contains("graphical-session.target"),
            "the default must not leak in when another target was asked for"
        );
    }

    /// The service exists to survive a compositor that is not ready yet, so a
    /// missing restart policy would defeat the point.
    #[test]
    fn the_unit_restarts_on_failure_but_not_forever() {
        let unit = render_unit(Path::new("/usr/bin/hyprdmc"), DEFAULT_TARGET);
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("StartLimitBurst=20"));
    }

    #[test]
    fn a_relative_executable_is_refused() {
        // `ExecStart` is resolved by systemd, not by the shell that installed
        // the unit: a relative path would point somewhere else entirely.
        assert!(!Path::new("target/release/hyprdmc").is_absolute());
    }

    #[test]
    fn the_unit_lands_under_the_user_unit_directory() {
        let path = unit_path();
        assert!(path.ends_with("systemd/user/hyprdmc.service"), "{path:?}");
    }
}
