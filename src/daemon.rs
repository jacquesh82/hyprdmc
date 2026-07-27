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
use crate::compositor::Compositor;
use crate::config::Config;
use crate::history::{Snapshot, Store, signature};
use crate::layout::Layout;
use crate::monitor::Monitor;
use crate::notify::{self, Urgency};
use crate::session::{CompositorEvent, Session};

/// A dock fires several events in a row; we wait for things to settle.
const DEBOUNCE: Duration = Duration::from_millis(500);
/// Reconnecting to the event stream: initial delay, then doubles.
const RECONNECT_MIN: Duration = Duration::from_millis(250);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Why a given layout was chosen.
///
/// Carried rather than reduced to a boolean because it is what the user gets
/// told: "profile desk applied" and "restored your previous arrangement" mean
/// very different things when a screen has just appeared.
#[derive(Debug, Clone, PartialEq)]
pub enum Origin {
    /// A named profile matched the connected outputs.
    Profile { name: String, layout: Layout },
    /// These exact screens had been arranged before; that arrangement is back.
    Recalled { layout: Layout },
    /// Nothing was known and the current state was unusable, so the outputs
    /// were laid out left to right.
    Arranged { layout: Layout },
    /// Nothing was known but the current state was fine: left alone.
    Unchanged { layout: Layout },
}

impl Origin {
    pub fn layout(&self) -> &Layout {
        match self {
            Origin::Profile { layout, .. }
            | Origin::Recalled { layout }
            | Origin::Arranged { layout }
            | Origin::Unchanged { layout } => layout,
        }
    }

    pub fn profile(&self) -> Option<&str> {
        match self {
            Origin::Profile { name, .. } => Some(name),
            _ => None,
        }
    }

    /// Sentence shown to the user, in their language.
    pub fn describe(&self) -> String {
        match self {
            Origin::Profile { name, .. } => t!("origin.profile", name = name).to_string(),
            Origin::Recalled { .. } => t!("origin.recalled").to_string(),
            Origin::Arranged { .. } => t!("origin.arranged").to_string(),
            Origin::Unchanged { .. } => t!("origin.unchanged").to_string(),
        }
    }

    /// Is this worth interrupting the user for?
    ///
    /// Leaving a working layout alone is not news; anything that moved their
    /// screens is.
    pub fn worth_notifying(&self) -> bool {
        !matches!(self, Origin::Unchanged { .. })
    }
}

/// Scheduled revert, waiting for confirmation.
struct PendingRevert {
    /// Last confirmed state, to revert to.
    previous: Layout,
    /// State awaiting confirmation. Kept here so that [`AppState::confirm`]
    /// knows what to file in the history: a layout only earns its place there
    /// once the user has said they can still see their screens.
    applied: Layout,
    /// Named profile this layout came from, if any.
    profile: Option<String>,
    timer: JoinHandle<()>,
}

/// State shared between the daemon, the web API, and one-off commands.
pub struct AppState {
    pub session: Arc<dyn Session>,
    /// The compositor plugin in force, resolved once at startup.
    ///
    /// Resolved once rather than per call: it is decided by the environment and
    /// by `config.toml`, neither of which changes under a running session, and a
    /// layout applied with one plugin then rolled back with another would be a
    /// bug with no good way to reproduce it.
    pub compositor: &'static (dyn Compositor + Sync),
    pub config: RwLock<Config>,
    /// Broadcasts the full state to SSE clients.
    pub events: broadcast::Sender<String>,
    /// History and per-output-set recall, persisted between runs.
    pub store: Mutex<Store>,
    pending: Mutex<Option<PendingRevert>>,
}

impl AppState {
    pub fn new(session: Arc<dyn Session>, config: Config) -> Arc<Self> {
        Self::with_store(session, config, Store::load())
    }

    /// Same, with an explicit store — used by tests so they never touch the
    /// user's real state file.
    pub fn with_store(session: Arc<dyn Session>, config: Config, store: Store) -> Arc<Self> {
        // A misconfigured plugin name must not take the daemon down: it is
        // reported by every command that loads the config, and falling back keeps
        // the user's displays working meanwhile.
        let compositor = config.compositor().unwrap_or_else(|err| {
            tracing::warn!("{err:#}; falling back to the detected compositor");
            crate::compositor::resolve(None).expect("the fallback never fails")
        });
        let (events, _) = broadcast::channel(16);
        Arc::new(Self {
            session,
            compositor,
            config: RwLock::new(config),
            events,
            store: Mutex::new(store),
            pending: Mutex::new(None),
        })
    }

    /// Reads output state without blocking the async executor.
    pub async fn monitors(&self) -> Result<Vec<Monitor>> {
        let session = Arc::clone(&self.session);
        tokio::task::spawn_blocking(move || session.outputs()).await?
    }

    /// Applies a layout, then arms the automatic revert if the user has
    /// left the safety net active.
    ///
    /// `profile` names the profile the layout came from, for the history;
    /// `None` means it was arranged by hand. Filing happens here (firm apply)
    /// or on confirmation (guarded apply) — never at both, and never for a
    /// layout Hyprland rolled back.
    pub async fn apply(
        self: &Arc<Self>,
        layout: Layout,
        force: bool,
        guard: bool,
        profile: Option<String>,
    ) -> Result<ApplyReport> {
        let session = Arc::clone(&self.session);
        let compositor = self.compositor;
        let previous = {
            let s = Arc::clone(&self.session);
            tokio::task::spawn_blocking(move || apply::snapshot(s.as_ref())).await??
        };

        let target = layout.clone();
        let report = tokio::task::spawn_blocking(move || {
            apply::apply(session.as_ref(), compositor, &target, force)
        })
        .await??;

        let timeout = Duration::from_secs(self.config.read().await.settings.confirm_timeout_secs);
        if guard && report.succeeded() && !timeout.is_zero() {
            self.arm_revert(previous, layout, profile, timeout).await;
        } else if report.succeeded() {
            // A firm apply: nobody will confirm it, so it is the new reference
            // and it goes into the history right away.
            self.cancel_revert().await;
            self.remember(&layout, profile.as_deref()).await;
        }

        self.broadcast().await;
        Ok(report)
    }

    /// Schedules the revert. If a revert was already armed, we keep **its**
    /// reference state: the last one confirmed by the user, not some
    /// intermediate state that was never validated.
    async fn arm_revert(
        self: &Arc<Self>,
        previous: Layout,
        applied: Layout,
        profile: Option<String>,
        timeout: Duration,
    ) {
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
            let session = Arc::clone(&state.session);
            let compositor = state.compositor;
            let layout = to_restore.clone();
            let _ = tokio::task::spawn_blocking(move || {
                apply::restore(session.as_ref(), compositor, &layout)
            })
            .await;
            state.pending.lock().await.take();
            state.broadcast().await;
        });

        *slot = Some(PendingRevert {
            previous,
            applied,
            profile,
            timer,
        });
    }

    /// Confirms the current configuration: the revert is disarmed, and the
    /// layout is filed in the history.
    ///
    /// Filing happens *here* rather than at apply time on purpose — the
    /// history is an undo list, and an arrangement the user rejected (or never
    /// answered for, and which reverted on its own) has no business in it.
    pub async fn confirm(&self) -> bool {
        let Some(pending) = self.pending.lock().await.take() else {
            return false;
        };
        pending.timer.abort();
        self.remember(&pending.applied, pending.profile.as_deref())
            .await;
        true
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
        let session = Arc::clone(&self.session);
        let compositor = self.compositor;
        tokio::task::spawn_blocking(move || {
            apply::restore(session.as_ref(), compositor, &p.previous)
        })
        .await??;
        self.broadcast().await;
        Ok(true)
    }

    /// Picks and applies the profile matching the connected hardware.
    ///
    /// Without a matching profile, we fall back to a simple horizontal
    /// arrangement: outputs side by side beat one stacked on top of another.
    pub async fn reconcile(self: &Arc<Self>) -> Result<Origin> {
        let monitors = self.monitors().await?;
        let origin = self.choose(&monitors).await?;
        tracing::info!("{}", origin.describe());

        // A firm apply: nobody is around to confirm a hotplug event. It files
        // itself in the history — but only if Hyprland kept it.
        self.apply(
            origin.layout().clone(),
            false,
            false,
            origin.profile().map(str::to_string),
        )
        .await?;
        Ok(origin)
    }

    /// Decides which layout to apply for the connected hardware.
    ///
    /// Order matters, and encodes what the user meant most recently:
    ///
    /// 1. a **named profile** — an explicit, deliberate choice;
    /// 2. the **layout last used with these exact screens** — implicit, but
    ///    still the user's own arrangement, and the reason plugging a dock
    ///    back in just works without configuring anything;
    /// 3. **automatic arrangement** — only when nothing is known and the
    ///    current state is unusable.
    pub async fn choose(&self, monitors: &[Monitor]) -> Result<Origin> {
        // The main screen is a standing choice, so it is stamped onto whichever
        // layout wins — including one recalled from the store, which may predate
        // the choice. Anchoring on it is only done where the layout is being
        // (re)positioned anyway: `Unchanged` means "these screens are fine where
        // they are", and moving them to honour an anchor would make a liar of it.
        let primary = self.config.read().await.primary_output(monitors);

        if let Some(profile) = self.config.read().await.best_match(monitors) {
            let mut layout = profile.resolve(monitors)?.with_primary(primary);
            layout.normalize();
            return Ok(Origin::Profile {
                name: profile.name.clone(),
                layout,
            });
        }

        if self.config.read().await.settings.remember
            && let Some(snapshot) = self.store.lock().await.recall_for(monitors)
        {
            return Ok(Origin::Recalled {
                layout: snapshot.layout.clone().with_primary(primary),
            });
        }

        let mut layout = Layout::from_monitors(monitors).with_primary(primary);
        if layout.has_errors() {
            layout.auto_arrange();
            return Ok(Origin::Arranged { layout });
        }
        Ok(Origin::Unchanged { layout })
    }

    /// Files a layout in the history and the recall map.
    ///
    /// Best-effort, like its CLI counterpart: failing to record must not turn
    /// a successful apply into an error, so problems are logged and swallowed.
    async fn remember(&self, layout: &Layout, profile: Option<&str>) {
        let Ok(monitors) = self.monitors().await else {
            tracing::warn!("outputs unreadable: layout not filed in the history");
            return;
        };
        let snapshot = Snapshot::new(
            layout.clone(),
            signature(&monitors),
            profile.map(str::to_string),
        );
        let mut store = self.store.lock().await;
        store.record(snapshot);
        if let Err(err) = store.save() {
            tracing::warn!("could not save the state: {err:#}");
        }
    }

    /// Reapplies a layout from the history. `0` is the most recent.
    pub async fn restore(self: &Arc<Self>, index: usize) -> Result<Snapshot> {
        let snapshot = self
            .store
            .lock()
            .await
            .entry(index)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no history entry at position {index}"))?;
        self.apply(
            snapshot.layout.clone(),
            false,
            true,
            snapshot.profile.clone(),
        )
        .await?;
        Ok(snapshot)
    }

    /// Full snapshot for the API and the SSE stream.
    pub async fn state_json(&self) -> Result<serde_json::Value> {
        let monitors = self.monitors().await?;
        let cfg = self.config.read().await;
        // The live layout carries the main screen the settings designate: the UI
        // edits a copy of this, so its idea of what is current has to include
        // the choice, or picking the screen that is already the main one would
        // read as a pending change.
        let layout = Layout::from_monitors(&monitors).with_primary(cfg.primary_output(&monitors));
        Ok(serde_json::json!({
            "monitors": monitors,
            "layout": layout,
            "issues": layout.validate(),
            "profiles": cfg.profiles,
            "activeProfile": cfg.best_match(&monitors).map(|p| p.name.clone()),
            "revertPending": self.pending.lock().await.is_some(),
            "confirmTimeoutSecs": cfg.settings.confirm_timeout_secs,
            // Enough for the UI to name the plugin, say what it can do, and
            // point at the file it writes — without a branch on the id.
            "compositor": {
                "id": self.compositor.id(),
                "label": self.compositor.label(),
                "supportsLive": self.compositor.drives_sessions(),
                "monitorsFile": self.compositor.monitors_file(),
                "inputFile": self.compositor.input_file(),
            },
            "history": self.store.lock().await.history,
        }))
    }

    /// Tells the user what changed and what was done about it.
    ///
    /// Goes to the desktop as a notification and to the SSE clients as state,
    /// so someone with the web UI open sees the same thing without a popup.
    pub async fn announce(&self, summary: &str, detail: &str) {
        tracing::info!("{summary}: {detail}");
        if self.config.read().await.settings.notifications {
            let (summary, detail) = (summary.to_string(), detail.to_string());
            // notify-send blocks until the notification daemon answers.
            tokio::task::spawn_blocking(move || notify::send(&summary, &detail, Urgency::Normal))
                .await
                .ok();
        }
        self.broadcast().await;
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

/// Main daemon loop. Only returns on a fatal error or a shutdown signal.
pub async fn run(state: Arc<AppState>) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<CompositorEvent>();

    // The event stream lives in its own task and reconnects on its own: the
    // compositor can restart without taking the daemon down with it.
    //
    // `EventStream::next_event` blocks, so the reading happens on a blocking task
    // and each event is forwarded into this channel. That is what lets a plugin
    // implement hotplug with no async code of its own — see [`crate::session`].
    let listener = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let mut backoff = RECONNECT_MIN;
            loop {
                let tx = tx.clone();
                let session = Arc::clone(&state.session);
                let outcome = tokio::task::spawn_blocking(move || -> Result<()> {
                    let mut stream = session.watch()?;
                    while let Some(event) = stream.next_event() {
                        // The receiver is gone: the daemon is shutting down.
                        if tx.send(event).is_err() {
                            return Ok(());
                        }
                    }
                    Ok(())
                })
                .await;

                match outcome {
                    Ok(Ok(())) => tracing::warn!("event stream closed by the compositor"),
                    Ok(Err(err)) => tracing::warn!("event stream unavailable: {err:#}"),
                    Err(err) => tracing::warn!("event reader stopped: {err:#}"),
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RECONNECT_MAX);
            }
        })
    };

    // Initial alignment on startup: the hardware may have changed while the
    // daemon was not running.
    if state.config.read().await.settings.auto_apply {
        match state.reconcile().await {
            // Startup is not a "change": only speak up if something moved.
            Ok(origin) if origin.worth_notifying() => {
                state
                    .announce(&t!("notify.startup"), &origin.describe())
                    .await;
            }
            Ok(_) => {}
            Err(err) => tracing::error!("initial alignment failed: {err:#}"),
        }
    }

    // Names seen during the debounce window, so the notification can say what
    // actually changed rather than just "something did".
    let mut changes = Changes::default();
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
                    if ev.affects_outputs() {
                        tracing::debug!("output event: {ev:?}");
                        changes.record(&ev);
                        deadline = Some(tokio::time::Instant::now() + DEBOUNCE);
                    }
                }
                None => break,
            },
            () = tick => {
                deadline = None;
                let summary = changes.take();
                if state.config.read().await.settings.auto_apply {
                    match state.reconcile().await {
                        Ok(origin) => state.announce(&summary, &origin.describe()).await,
                        Err(err) => {
                            tracing::error!("could not react to hotplug: {err:#}");
                            state.announce(&summary, &t!("notify.failed")).await;
                        }
                    }
                } else {
                    state.announce(&summary, &t!("notify.manual")).await;
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
///
/// The plugin decides how to reach the compositor, so this works the same
/// whichever one is running — and the error, when there is one, names it.
pub fn bootstrap() -> Result<Arc<AppState>> {
    let config = Config::load()?;
    let compositor = config.compositor()?;
    let session = compositor
        .connect()
        .with_context(|| t!("compositor.unreachable", name = compositor.label()).to_string())?;
    Ok(AppState::new(Arc::from(session), config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::hyprland::ipc::fake::FakeSession;

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
        // An ephemeral store: tests must never read or write the real one.
        AppState::with_store(
            Arc::new(FakeSession::with_monitors(json)),
            config,
            Store::ephemeral(),
        )
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
        // Two adjacent screens are already a usable layout: nothing to do.
        let origin = state.reconcile().await.unwrap();
        assert!(matches!(origin, Origin::Unchanged { .. }));
        assert!(!origin.worth_notifying());
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
        let origin = state.reconcile().await.unwrap();
        assert_eq!(origin.profile(), Some("desk"));
        assert!(origin.worth_notifying());
    }

    #[tokio::test]
    async fn guarded_apply_arms_a_revert_that_confirm_cancels() {
        let mut cfg = Config::default();
        cfg.settings.confirm_timeout_secs = 60;
        let state = state_with(&json_two_screens(), cfg);

        let monitors = state.monitors().await.unwrap();
        let layout = Layout::from_monitors(&monitors);
        state.apply(layout, false, true, None).await.unwrap();

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
        state.apply(layout, false, false, None).await.unwrap();
        assert!(!state.revert_pending().await);
    }

    #[tokio::test]
    async fn a_second_apply_still_reverts_to_the_last_confirmed_state() {
        let mut cfg = Config::default();
        cfg.settings.confirm_timeout_secs = 60;
        let state = state_with(&json_two_screens(), cfg);

        let original = Layout::from_monitors(&state.monitors().await.unwrap());
        state
            .apply(original.clone(), false, true, None)
            .await
            .unwrap();
        state
            .apply(original.clone(), false, true, None)
            .await
            .unwrap();

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
        state.apply(layout, false, true, None).await.unwrap();

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
        state.apply(layout, false, true, None).await.unwrap();
        assert!(state.revert_pending().await);

        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(
            !state.revert_pending().await,
            "the revert should have triggered on its own"
        );
    }
}

/// Outputs that appeared or disappeared during a debounce window.
///
/// A dock emits several events in a row; collecting them lets the daemon
/// report "2 displays connected" instead of one popup per event.
#[derive(Debug, Default)]
struct Changes {
    added: Vec<String>,
    removed: Vec<String>,
}

impl Changes {
    fn record(&mut self, event: &CompositorEvent) {
        match event {
            CompositorEvent::OutputAdded(name) if !self.added.contains(name) => {
                self.added.push(name.clone())
            }
            CompositorEvent::OutputRemoved(name) if !self.removed.contains(name) => {
                self.removed.push(name.clone())
            }
            _ => {}
        }
    }

    /// Consumes the batch and renders it as a sentence.
    fn take(&mut self) -> String {
        let added = std::mem::take(&mut self.added);
        let removed = std::mem::take(&mut self.removed);
        let mut parts = Vec::new();
        if !added.is_empty() {
            parts.push(t!("notify.connected", outputs = added.join(", ")).to_string());
        }
        if !removed.is_empty() {
            parts.push(t!("notify.disconnected", outputs = removed.join(", ")).to_string());
        }
        if parts.is_empty() {
            return t!("notify.changed").to_string();
        }
        parts.join(" · ")
    }
}

#[cfg(test)]
mod recall_tests {
    use super::*;
    use crate::compositor::hyprland::ipc::fake::FakeSession;
    use crate::history::Store;

    /// Two adjacent screens — a layout that needs no fixing.
    const TWO: &str = r#"[
      {"id":0,"name":"eDP-1","make":"AU","model":"X","serial":"L1","width":1920,"height":1080,
       "refreshRate":60.0,"x":0,"y":0,"scale":1.0,"transform":0,"disabled":false,
       "mirrorOf":"none","availableModes":["1920x1080@60.00Hz"]},
      {"id":1,"name":"DP-1","make":"Dell","model":"U","serial":"D1","width":1920,"height":1080,
       "refreshRate":60.0,"x":1920,"y":0,"scale":1.0,"transform":0,"disabled":false,
       "mirrorOf":"none","availableModes":["1920x1080@60.00Hz"]}
    ]"#;

    fn state(store: Store, config: Config) -> Arc<AppState> {
        AppState::with_store(Arc::new(FakeSession::with_monitors(TWO)), config, store)
    }

    #[tokio::test]
    async fn a_known_output_set_recalls_its_layout() {
        let state = state(Store::ephemeral(), Config::default());
        let monitors = state.monitors().await.unwrap();

        // Pretend the user once stacked the screens vertically.
        let mut stacked = Layout::from_monitors(&monitors);
        stacked.get_mut("DP-1").unwrap().x = 0;
        stacked.get_mut("DP-1").unwrap().y = 1080;
        state
            .store
            .lock()
            .await
            .record(Snapshot::new(stacked.clone(), signature(&monitors), None));

        let origin = state.choose(&monitors).await.unwrap();
        assert!(matches!(origin, Origin::Recalled { .. }));
        assert_eq!(origin.layout(), &stacked);
    }

    #[tokio::test]
    async fn a_named_profile_outranks_the_recalled_layout() {
        // An explicit choice must win over an implicit one.
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
        let state = state(Store::ephemeral(), cfg);
        let monitors = state.monitors().await.unwrap();

        let mut stacked = Layout::from_monitors(&monitors);
        stacked.get_mut("DP-1").unwrap().y = 1080;
        state
            .store
            .lock()
            .await
            .record(Snapshot::new(stacked, signature(&monitors), None));

        assert_eq!(
            state.choose(&monitors).await.unwrap().profile(),
            Some("desk")
        );
    }

    #[tokio::test]
    async fn the_main_screen_is_stamped_on_whatever_layout_wins() {
        // Including a layout recalled from the store, which was recorded before
        // the choice existed: the setting is the current truth, the snapshot is
        // only a memory of where the screens were.
        let mut cfg = Config::default();
        cfg.settings.primary = Some("Dell*".into());
        let state = state(Store::ephemeral(), cfg);
        let monitors = state.monitors().await.unwrap();

        let stacked = Layout::from_monitors(&monitors);
        assert_eq!(stacked.primary, None, "the live state knows nothing of it");
        state
            .store
            .lock()
            .await
            .record(Snapshot::new(stacked, signature(&monitors), None));

        let origin = state.choose(&monitors).await.unwrap();
        assert!(matches!(origin, Origin::Recalled { .. }));
        assert_eq!(origin.layout().primary.as_deref(), Some("DP-1"));
    }

    #[tokio::test]
    async fn a_profile_is_anchored_on_the_main_screen() {
        // The profile puts eDP-1 at 0x0 and the Dell to its right; naming the
        // Dell as the main screen moves the origin onto it, so the laptop panel
        // ends up to its left at a negative coordinate.
        let mut cfg: Config = toml::from_str(
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
        cfg.settings.primary = Some("Dell*".into());
        let state = state(Store::ephemeral(), cfg);
        let monitors = state.monitors().await.unwrap();

        let origin = state.choose(&monitors).await.unwrap();
        assert_eq!(origin.profile(), Some("desk"));
        let layout = origin.layout();
        assert_eq!(layout.get("DP-1").unwrap().x, 0);
        assert_eq!(layout.get("eDP-1").unwrap().x, -1920);
        assert!(!layout.has_errors());
    }

    #[tokio::test]
    async fn recall_can_be_switched_off() {
        let mut cfg = Config::default();
        cfg.settings.remember = false;
        let state = state(Store::ephemeral(), cfg);
        let monitors = state.monitors().await.unwrap();

        let mut stacked = Layout::from_monitors(&monitors);
        stacked.get_mut("DP-1").unwrap().y = 1080;
        state
            .store
            .lock()
            .await
            .record(Snapshot::new(stacked, signature(&monitors), None));

        let origin = state.choose(&monitors).await.unwrap();
        assert!(matches!(origin, Origin::Unchanged { .. }));
    }

    #[tokio::test]
    async fn an_unknown_output_set_is_left_alone_when_already_usable() {
        let state = state(Store::ephemeral(), Config::default());
        let monitors = state.monitors().await.unwrap();
        let origin = state.choose(&monitors).await.unwrap();
        assert!(matches!(origin, Origin::Unchanged { .. }));
    }

    #[tokio::test]
    async fn reconcile_files_what_it_applied() {
        let state = state(Store::ephemeral(), Config::default());
        state.reconcile().await.unwrap();

        let store = state.store.lock().await;
        assert_eq!(store.history.len(), 1);
        assert_eq!(store.recall.len(), 1);
    }

    #[tokio::test]
    async fn restore_reapplies_a_history_entry() {
        let state = state(Store::ephemeral(), Config::default());
        let monitors = state.monitors().await.unwrap();
        let layout = Layout::from_monitors(&monitors);
        state
            .store
            .lock()
            .await
            .record(Snapshot::new(layout.clone(), signature(&monitors), None));

        let restored = state.restore(0).await.unwrap();
        assert_eq!(restored.layout, layout);
        assert!(state.restore(9).await.is_err());
    }

    /// The web UI is the main user of the guarded path: what it applies has to
    /// end up in the history like anything the CLI applies, or "restore the
    /// previous arrangement" silently ignores everything done with the mouse.
    #[tokio::test]
    async fn a_guarded_apply_is_filed_once_confirmed() {
        let mut cfg = Config::default();
        cfg.settings.confirm_timeout_secs = 60;
        let state = state(Store::ephemeral(), cfg);

        // The layout the fake backend already reports: anything else reads back
        // as drift and gets rolled back, which is a different test.
        let applied = Layout::from_monitors(&state.monitors().await.unwrap());
        state
            .apply(applied.clone(), false, true, None)
            .await
            .unwrap();

        assert!(
            state.store.lock().await.history.is_empty(),
            "an unconfirmed layout is not history yet"
        );

        assert!(state.confirm().await);
        let store = state.store.lock().await;
        assert_eq!(store.history.len(), 1);
        assert_eq!(store.history[0].layout, applied);
        // The recall map too: this is what makes redocking reuse it.
        assert_eq!(store.recall.len(), 1);
    }

    #[tokio::test]
    async fn a_reverted_layout_never_reaches_the_history() {
        let mut cfg = Config::default();
        cfg.settings.confirm_timeout_secs = 60;
        let state = state(Store::ephemeral(), cfg);

        // The layout the fake backend already reports: anything else reads back
        // as drift and gets rolled back, which is a different test.
        let applied = Layout::from_monitors(&state.monitors().await.unwrap());
        state.apply(applied, false, true, None).await.unwrap();
        assert!(state.revert_now().await.unwrap());

        assert!(
            state.store.lock().await.history.is_empty(),
            "an arrangement the user rejected has no business in an undo list"
        );
    }

    #[tokio::test]
    async fn a_guard_left_to_expire_files_nothing() {
        let mut cfg = Config::default();
        cfg.settings.confirm_timeout_secs = 1;
        let state = state(Store::ephemeral(), cfg);

        // The layout the fake backend already reports: anything else reads back
        // as drift and gets rolled back, which is a different test.
        let applied = Layout::from_monitors(&state.monitors().await.unwrap());
        state.apply(applied, false, true, None).await.unwrap();

        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(!state.revert_pending().await);
        assert!(
            state.store.lock().await.history.is_empty(),
            "no answer means no, and no is not history"
        );
    }

    #[tokio::test]
    async fn an_unguarded_apply_is_filed_straight_away() {
        let state = state(Store::ephemeral(), Config::default());
        // The layout the fake backend already reports: anything else reads back
        // as drift and gets rolled back, which is a different test.
        let applied = Layout::from_monitors(&state.monitors().await.unwrap());
        state
            .apply(applied.clone(), false, false, None)
            .await
            .unwrap();

        let store = state.store.lock().await;
        assert_eq!(store.history.len(), 1);
        assert_eq!(store.history[0].layout, applied);
    }
}
