//! Opening the web UI in the user's browser.
//!
//! Best-effort by design: failing to launch a browser must never bring down
//! the server the user asked for. Every failure path degrades to printing the
//! URL and carrying on.

use std::process::{Command, Stdio};

use anyhow::{Result, bail};

/// Is there a graphical session to open a browser into?
///
/// Guards against the daemon being started by systemd before the session is
/// up, where `xdg-open` would fail with a confusing message.
pub fn has_display() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
}

/// Launches a browser on `url`, detached from this process.
pub fn open(url: &str) -> Result<()> {
    if !has_display() {
        bail!("no graphical session (neither WAYLAND_DISPLAY nor DISPLAY is set)");
    }

    let (program, args) = launcher(std::env::var("BROWSER").ok().as_deref(), url);
    let mut child = Command::new(&program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not run {program}: {e}"))?;

    // The browser outlives us; reaping it in a side thread keeps it from
    // lingering as a zombie for the whole life of the daemon.
    std::thread::spawn(move || {
        let _ = child.wait();
    });

    Ok(())
}

/// Builds the command line used to open `url`.
///
/// Honours `$BROWSER`, including the convention where `%s` marks where the URL
/// goes — some wrappers need it in the middle of their arguments. Falls back
/// to `xdg-open`, which every desktop portal on Linux implements.
fn launcher(browser: Option<&str>, url: &str) -> (String, Vec<String>) {
    let Some(spec) = browser.map(str::trim).filter(|s| !s.is_empty()) else {
        return ("xdg-open".to_string(), vec![url.to_string()]);
    };

    let mut parts = spec.split_whitespace().map(str::to_string);
    let program = parts.next().expect("non-empty after the filter above");
    let mut args: Vec<String> = parts.collect();

    if args.iter().any(|a| a.contains("%s")) {
        for arg in &mut args {
            *arg = arg.replace("%s", url);
        }
    } else {
        args.push(url.to_string());
    }

    (program, args)
}

/// Address a browser on this machine can actually reach.
///
/// Binding to `0.0.0.0` or `::` means "every interface", which is not a
/// destination: the browser gets sent to the loopback instead.
pub fn reachable_url(addr: std::net::SocketAddr) -> String {
    if addr.ip().is_unspecified() {
        format!("http://127.0.0.1:{}", addr.port())
    } else {
        format!("http://{addr}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    #[test]
    fn falls_back_to_xdg_open() {
        assert_eq!(
            launcher(None, "http://x"),
            ("xdg-open".to_string(), vec!["http://x".to_string()])
        );
    }

    #[test]
    fn blank_browser_variable_is_ignored() {
        // BROWSER="" is common in stripped environments; it must not become
        // an attempt to execute the empty program.
        assert_eq!(launcher(Some("   "), "http://x").0, "xdg-open");
        assert_eq!(launcher(Some(""), "http://x").0, "xdg-open");
    }

    #[test]
    fn browser_variable_is_honoured() {
        assert_eq!(
            launcher(Some("firefox"), "http://x"),
            ("firefox".to_string(), vec!["http://x".to_string()])
        );
    }

    #[test]
    fn browser_variable_may_carry_arguments() {
        let (program, args) = launcher(Some("firefox --new-window"), "http://x");
        assert_eq!(program, "firefox");
        assert_eq!(args, vec!["--new-window", "http://x"]);
    }

    #[test]
    fn percent_s_marks_where_the_url_goes() {
        // Wrappers that need the URL before their own flags rely on this.
        let (program, args) = launcher(Some("my-wrapper %s --kiosk"), "http://x");
        assert_eq!(program, "my-wrapper");
        assert_eq!(args, vec!["http://x", "--kiosk"]);
    }

    #[test]
    fn wildcard_bind_sends_the_browser_to_loopback() {
        let any: SocketAddr = "0.0.0.0:8787".parse().unwrap();
        assert_eq!(reachable_url(any), "http://127.0.0.1:8787");
        let any6: SocketAddr = "[::]:8787".parse().unwrap();
        assert_eq!(reachable_url(any6), "http://127.0.0.1:8787");
    }

    #[test]
    fn concrete_bind_is_used_as_is() {
        let local: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        assert_eq!(reachable_url(local), "http://127.0.0.1:9000");
    }
}
