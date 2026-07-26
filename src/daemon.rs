//! Daemon: shared state, hotplug reaction, deferred revert.
//!
//! This is the component that "dynamically maintains" the configuration: it
//! listens to Hyprland's event stream and reapplies the profile that
//! matches the currently connected hardware whenever it changes.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rust_i18n::t;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::apply::{self, ApplyReport};
use crate::config::Config;
use crate::ipc::{HyprBackend, HyprEvent, HyprSocket};
use crate::layout::Layout;
use crate::monitor::Monitor;

/// A dock fires several events in a row; we wait for things to settle.
const DEBOUNCE: Duration = Duration::from_millis(500);
/// Reconnecting to the event stream: initial delay, then doubles.
const RECONNECT_MIN: Duration = Duration::from_millis(250);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Scheduled revert, waiting for confirmation.
struct PendingRevert {
    /// Last confirmed state, to revert to.
    previous: Layout,
    timer: JoinHandle<()>,
}

/// State shared between the daemon, the web API, and one-off commands.
pub struct AppState {
    pub backend: Arc<dyn HyprBackend>,
    pub config: RwLock<Config>,
    /// Broadcasts the full state to SSE clients.
    pub events: broadcast::Sender<String>,
    pending: Mutex<Option<PendingRevert>>,
}

impl AppState {
    pub fn new(backend: Arc<dyn HyprBackend>, config: Config) -> Arc<Self> {
        let (events, _) = broadcast::channel(16);
        Arc::new(Self {
            backend,
            config: RwLock::new(config),
            events,
            pending: Mutex::new(None),
        })
    }

    /// Reads output state without blocking the async executor.
    pub async fn monitors(&self) -> Result<Vec<Monitor>> {
        let backend = Arc::clone(&self.backend);
        tokio::task::spawn_blocking(move || backend.monitors()).await?
    }

    /// Applies a layout, then arms the automatic revert if the user has
    /// left the safety net active.
    pub async fn apply(
        self: &Arc<Self>,
        layout: Layout,
        force: bool,
        guard: bool,
    ) -> Result<ApplyReport> {
        let backend = Arc::clone(&self.backend);
        let previous = {
            let b = Arc::clone(&self.backend);
            tokio::task::spawn_blocking(move || apply::snapshot(b.as_ref())).await??
        };

        let target = layout.clone();
        let report =
            tokio::task::spawn_blocking(move || apply::apply(backend.as_ref(), &target, force))
                .await??;

        let timeout = Duration::from_secs(self.config.read().await.settings.confirm_timeout_secs);
        if guard && report.succeeded() && !timeout.is_zero() {
            self.arm_revert(previous, timeout).await;
        } else if report.succeeded() {
            // A firm apply: the current state becomes the new reference.
            self.cancel_revert().await;
        }

        self.broadcast().await;
        Ok(report)
    }

    /// Schedules the revert. If a revert was already armed, we keep **its**
    /// reference state: the last one confirmed by the user, not some
    /// intermediate state that was never validated.
    async fn arm_revert(self: &Arc<Self>, previous: Layout, timeout: Duration) {
        let mut slot = self.pending.lock().await;
        let previous = match slot.take() {
            Some(old) => {
                old.timer.abort();
                old.previous
            }
            None => previous,
        };

        let state = Arc::clone(self);
        let to_restore = previous.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            tracing::warn!("no confirmation received: reverting to the previous configuration");
            let backend = Arc::clone(&state.backend);
            let layout = to_restore.clone();
            let _ = tokio::task::spawn_blocking(move || apply::restore(backend.as_ref(), &layout))
                .await;
            state.pending.lock().await.take();
            state.broadcast().await;
        });

        *slot = Some(PendingRevert { previous, timer });
    }

    /// Confirms the current configuration: the revert is disarmed.
    pub async fn confirm(&self) -> bool {
        self.cancel_revert().await
    }

    async fn cancel_revert(&self) -> bool {
        match self.pending.lock().await.take() {
            Some(p) => {
                p.timer.abort();
                true
            }
            None => false,
        }
    }

    /// Is a revert pending confirmation?
    pub async fn revert_pending(&self) -> bool {
        self.pending.lock().await.is_some()
    }

    /// Reverts immediately to the last confirmed state.
    pub async fn revert_now(&self) -> Result<bool> {
        let Some(p) = self.pending.lock().await.take() else {
            return Ok(false);
        };
        p.timer.abort();
        let backend = Arc::clone(&self.backend);
        tokio::task::spawn_blocking(move || apply::restore(backend.as_ref(), &p.previous))
            .await??;
        self.broadcast().await;
        Ok(true)
    }

    /// Picks and applies the profile matching the connected hardware.
    ///
    /// Without a matching profile, we fall back to a simple horizontal
    /// arrangement: outputs side by side beat one stacked on top of another.
    pub async fn reconcile(self: &Arc<Self>) -> Result<Option<String>> {
        let monitors = self.monitors().await?;
        let (name, layout) = {
            let cfg = self.config.read().await;
            match cfg.best_match(&monitors) {
                Some(profile) => (Some(profile.name.clone()), profile.resolve(&monitors)?),
                None => {
                    let mut layout = Layout::from_monitors(&monitors);
                    if layout.has_errors() {
                        tracing::info!("no profile matches: arranging automatically");
                        layout.auto_arrange();
                    }
                    (None, layout)
                }
            }
        };

        match &name {
            Some(n) => tracing::info!("applying profile \"{n}\""),
            None => tracing::debug!("no matching profile"),
        }

        // A firm apply: nobody is around to confirm a hotplug event.
        self.apply(layout, false, false).await?;
        Ok(name)
    }

    /// Full snapshot for the API and the SSE stream.
    pub async fn state_json(&self) -> Result<serde_json::Value> {
        let monitors = self.monitors().await?;
        let layout = Layout::from_monitors(&monitors);
        let cfg = self.config.read().await;
        Ok(serde_json::json!({
            "monitors": monitors,
            "layout": layout,
            "issues": layout.validate(),
            "profiles": cfg.profiles,
            "activeProfile": cfg.best_match(&monitors).map(|p| p.name.clone()),
            "revertPending": self.pending.lock().await.is_some(),
            "confirmTimeoutSecs": cfg.settings.confirm_timeout_secs,
        }))
    }

    /// Pushes the current state to connected clients.
    pub async fn broadcast(&self) {
        if self.events.receiver_count() == 0 {
            return;
        }
        match self.state_json().await {
            Ok(value) => {
                let _ = self.events.send(value.to_string());
            }
            Err(err) => tracing::warn!("could not broadcast state: {err:#}"),
        }
    }
}

/// Main daemon loop.
///
/// `socket` is the path to `.socket2.sock`. The function only returns on a
/// fatal error or a shutdown signal.
pub async fn run(state: Arc<AppState>, hypr: &HyprSocket) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<HyprEvent>();
    let socket = hypr.event_socket();

    // The event stream lives in its own task and reconnects on its own:
    // Hyprland can restart without taking the daemon down with it.
    let listener = tokio::spawn(async move {
        let mut backoff = RECONNECT_MIN;
        loop {
            let tx = tx.clone();
            match crate::ipc::stream_events(&socket, move |ev| {
                let _ = tx.send(ev);
                Ok(())
            })
            .await
            {
                Ok(()) => tracing::warn!("event stream closed by Hyprland"),
                Err(err) => tracing::warn!("event stream unavailable: {err:#}"),
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(RECONNECT_MAX);
        }
    });

    // Initial alignment on startup: the hardware may have changed while the
    // daemon was not running.
    if state.config.read().await.settings.auto_apply
        && let Err(err) = state.reconcile().await
    {
        tracing::error!("initial alignment failed: {err:#}");
    }

    let mut deadline: Option<tokio::time::Instant> = None;
    loop {
        let tick = async {
            match deadline {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            event = rx.recv() => match event {
                Some(ev) => {
                    if ev.affects_monitors() {
                        tracing::debug!("output event: {ev:?}");
                        deadline = Some(tokio::time::Instant::now() + DEBOUNCE);
                    }
                }
                None => break,
            },
            () = tick => {
                deadline = None;
                if state.config.read().await.settings.auto_apply {
                    if let Err(err) = state.reconcile().await {
                        tracing::error!("could not react to hotplug: {err:#}");
                    }
                } else {
                    state.broadcast().await;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("shutdown requested");
                break;
            }
        }
    }

    listener.abort();
    Ok(())
}

/// Builds the shared state from the configuration on disk.
pub fn bootstrap() -> Result<(Arc<AppState>, HyprSocket)> {
    let hypr = HyprSocket::connect().context(t!("ipc.unreachable").to_string())?;
    let config = Config::load()?;
    let state = AppState::new(Arc::new(hypr.clone()), config);
    Ok((state, hypr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::fake::FakeBackend;

    fn json_two_screens() -> String {
        r#"[
          {"id":0,"name":"eDP-1","description":"AU Optronics 0x5799","make":"AU Optronics",
           "model":"0x5799","serial":"","width":1920,"height":1080,"refreshRate":60.0,
           "x":0,"y":0,"scale":1.0,"transform":0,"focused":true,"disabled":false,
           "mirrorOf":"none","vrr":false,"availableModes":["1920x1080@60.00Hz"]},
          {"id":1,"name":"DP-1","description":"Dell U2723QE","make":"Dell","model":"U2723QE",
           "serial":"ABC","width":1920,"height":1080,"refreshRate":60.0,
           "x":1920,"y":0,"scale":1.0,"transform":0,"focused":false,"disabled":false,
           "mirrorOf":"none","vrr":false,"availableModes":["1920x1080@60.00Hz"]}
        ]"#
        .to_string()
    }

    fn state_with(json: &str, config: Config) -> Arc<AppState> {
        AppState::new(Arc::new(FakeBackend::with_monitors(json)), config)
    }

    #[tokio::test]
    async fn state_json_exposes_everything_the_ui_needs() {
        let state = state_with(&json_two_screens(), Config::default());
        let value = state.state_json().await.unwrap();
        assert_eq!(value["monitors"].as_array().unwrap().len(), 2);
        assert_eq!(value["layout"]["outputs"].as_array().unwrap().len(), 2);
        assert!(value["activeProfile"].is_null());
        assert_eq!(value["revertPending"], false);
    }

    #[tokio::test]
    async fn reconcile_without_profile_keeps_a_valid_layout() {
        let state = state_with(&json_two_screens(), Config::default());
        assert_eq!(state.reconcile().await.unwrap(), None);
    }

    #[tokio::test]
    async fn reconcile_picks_the_matching_profile() {
        let cfg: Config = toml::from_str(
            r#"
            [[profile]]
            name = "desk"
            [[profile.output]]
            match = "eDP-1"
            position = "0x0"
            [[profile.output]]
            match = "Dell*"
            position = "1920x0"
            "#,
        )
        .unwrap();
        let state = state_with(&json_two_screens(), cfg);
        assert_eq!(state.reconcile().await.unwrap().as_deref(), Some("desk"));
    }

    #[tokio::test]
    async fn guarded_apply_arms_a_revert_that_confirm_cancels() {
        let mut cfg = Config::default();
        cfg.settings.confirm_timeout_secs = 60;
        let state = state_with(&json_two_screens(), cfg);

        let monitors = state.monitors().await.unwrap();
        let layout = Layout::from_monitors(&monitors);
        state.apply(layout, false, true).await.unwrap();

        assert!(state.revert_pending().await);
        assert!(state.confirm().await);
        assert!(!state.revert_pending().await);
    }

    #[tokio::test]
    async fn unguarded_apply_leaves_nothing_pending() {
        let mut cfg = Config::default();
        cfg.settings.confirm_timeout_secs = 60;
        let state = state_with(&json_two_screens(), cfg);
        let layout = Layout::from_monitors(&state.monitors().await.unwrap());
        state.apply(layout, false, false).await.unwrap();
        assert!(!state.revert_pending().await);
    }

    #[tokio::test]
    async fn a_second_apply_still_reverts_to_the_last_confirmed_state() {
        let mut cfg = Config::default();
        cfg.settings.confirm_timeout_secs = 60;
        let state = state_with(&json_two_screens(), cfg);

        let original = Layout::from_monitors(&state.monitors().await.unwrap());
        state.apply(original.clone(), false, true).await.unwrap();
        state.apply(original.clone(), false, true).await.unwrap();

        // The revert point stays the first one, never an intermediate state.
        let pending = state.pending.lock().await;
        assert_eq!(pending.as_ref().unwrap().previous, original);
    }

    #[tokio::test]
    async fn revert_now_restores_immediately() {
        let mut cfg = Config::default();
        cfg.settings.confirm_timeout_secs = 60;
        let state = state_with(&json_two_screens(), cfg);
        let layout = Layout::from_monitors(&state.monitors().await.unwrap());
        state.apply(layout, false, true).await.unwrap();

        assert!(state.revert_now().await.unwrap());
        assert!(!state.revert_pending().await);
        // Nothing pending: a second call does nothing.
        assert!(!state.revert_now().await.unwrap());
    }

    #[tokio::test]
    async fn expired_guard_restores_the_previous_state() {
        let mut cfg = Config::default();
        cfg.settings.confirm_timeout_secs = 1;
        let state = state_with(&json_two_screens(), cfg);
        let layout = Layout::from_monitors(&state.monitors().await.unwrap());
        state.apply(layout, false, true).await.unwrap();
        assert!(state.revert_pending().await);

        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(
            !state.revert_pending().await,
            "the revert should have triggered on its own"
        );
    }
}
