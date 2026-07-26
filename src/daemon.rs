//! Démon : état partagé, réaction au branchement à chaud, retour arrière
//! différé.
//!
//! C'est le composant qui « maintient dynamiquement » la configuration : il
//! écoute le flux d'événements de Hyprland et réapplique le profil qui
//! correspond au matériel présent dès qu'il change.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::apply::{self, ApplyReport};
use crate::config::Config;
use crate::ipc::{HyprBackend, HyprEvent, HyprSocket};
use crate::layout::Layout;
use crate::monitor::Monitor;

/// Un dock émet plusieurs événements d'affilée ; on attend que ça se calme.
const DEBOUNCE: Duration = Duration::from_millis(500);
/// Reconnexion au flux d'événements : premier délai, puis doublement.
const RECONNECT_MIN: Duration = Duration::from_millis(250);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Retour arrière programmé, en attente de confirmation.
struct PendingRevert {
    /// Dernier état confirmé, vers lequel revenir.
    previous: Layout,
    timer: JoinHandle<()>,
}

/// État partagé entre le démon, l'API web et les commandes ponctuelles.
pub struct AppState {
    pub backend: Arc<dyn HyprBackend>,
    pub config: RwLock<Config>,
    /// Diffusion de l'état complet vers les clients SSE.
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

    /// Lit l'état des écrans sans bloquer l'exécuteur asynchrone.
    pub async fn monitors(&self) -> Result<Vec<Monitor>> {
        let backend = Arc::clone(&self.backend);
        tokio::task::spawn_blocking(move || backend.monitors()).await?
    }

    /// Applique un agencement, puis arme le retour arrière automatique si
    /// l'utilisateur a laissé le filet de sécurité actif.
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
            // Application ferme : l'état courant devient la nouvelle référence.
            self.cancel_revert().await;
        }

        self.broadcast().await;
        Ok(report)
    }

    /// Programme le retour arrière. Si un retour était déjà armé, on conserve
    /// **son** état de référence : c'est le dernier état confirmé par
    /// l'utilisateur, pas un état intermédiaire jamais validé.
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
            tracing::warn!("aucune confirmation reçue : retour à la configuration précédente");
            let backend = Arc::clone(&state.backend);
            let layout = to_restore.clone();
            let _ = tokio::task::spawn_blocking(move || apply::restore(backend.as_ref(), &layout))
                .await;
            state.pending.lock().await.take();
            state.broadcast().await;
        });

        *slot = Some(PendingRevert { previous, timer });
    }

    /// Confirme la configuration courante : le retour arrière est désarmé.
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

    /// Un retour arrière est-il en attente de confirmation ?
    pub async fn revert_pending(&self) -> bool {
        self.pending.lock().await.is_some()
    }

    /// Revient immédiatement au dernier état confirmé.
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

    /// Choisit et applique le profil correspondant au matériel branché.
    ///
    /// Sans profil correspondant, on se contente d'un rangement horizontal :
    /// mieux vaut des écrans côte à côte qu'un écran empilé sur un autre.
    pub async fn reconcile(self: &Arc<Self>) -> Result<Option<String>> {
        let monitors = self.monitors().await?;
        let (name, layout) = {
            let cfg = self.config.read().await;
            match cfg.best_match(&monitors) {
                Some(profile) => (Some(profile.name.clone()), profile.resolve(&monitors)?),
                None => {
                    let mut layout = Layout::from_monitors(&monitors);
                    if layout.has_errors() {
                        tracing::info!("aucun profil ne correspond : rangement automatique");
                        layout.auto_arrange();
                    }
                    (None, layout)
                }
            }
        };

        match &name {
            Some(n) => tracing::info!("application du profil « {n} »"),
            None => tracing::debug!("aucun profil correspondant"),
        }

        // Application ferme : personne n'est là pour confirmer un branchement.
        self.apply(layout, false, false).await?;
        Ok(name)
    }

    /// Instantané complet destiné à l'API et au flux SSE.
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

    /// Pousse l'état courant vers les clients connectés.
    pub async fn broadcast(&self) {
        if self.events.receiver_count() == 0 {
            return;
        }
        match self.state_json().await {
            Ok(value) => {
                let _ = self.events.send(value.to_string());
            }
            Err(err) => tracing::warn!("état non diffusable : {err:#}"),
        }
    }
}

/// Boucle principale du démon.
///
/// `socket` est le chemin de `.socket2.sock`. La fonction ne rend la main que
/// sur erreur fatale ou signal d'arrêt.
pub async fn run(state: Arc<AppState>, hypr: &HyprSocket) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<HyprEvent>();
    let socket = hypr.event_socket();

    // Le flux d'événements vit dans sa propre tâche et se reconnecte tout seul :
    // Hyprland peut redémarrer sans emporter le démon.
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
                Ok(()) => tracing::warn!("flux d'événements fermé par Hyprland"),
                Err(err) => tracing::warn!("flux d'événements indisponible : {err:#}"),
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(RECONNECT_MAX);
        }
    });

    // Premier alignement au démarrage : le matériel a pu changer pendant que le
    // démon ne tournait pas.
    if state.config.read().await.settings.auto_apply
        && let Err(err) = state.reconcile().await
    {
        tracing::error!("alignement initial impossible : {err:#}");
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
                        tracing::debug!("événement écran : {ev:?}");
                        deadline = Some(tokio::time::Instant::now() + DEBOUNCE);
                    }
                }
                None => break,
            },
            () = tick => {
                deadline = None;
                if state.config.read().await.settings.auto_apply {
                    if let Err(err) = state.reconcile().await {
                        tracing::error!("réaction au branchement impossible : {err:#}");
                    }
                } else {
                    state.broadcast().await;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("arrêt demandé");
                break;
            }
        }
    }

    listener.abort();
    Ok(())
}

/// Construit l'état partagé à partir de la configuration sur disque.
pub fn bootstrap() -> Result<(Arc<AppState>, HyprSocket)> {
    let hypr = HyprSocket::connect().context("Hyprland introuvable")?;
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
            name = "bureau"
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
        assert_eq!(state.reconcile().await.unwrap().as_deref(), Some("bureau"));
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

        // Le point de retour reste le premier, jamais un état intermédiaire.
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
        // Rien en attente : un second appel ne fait rien.
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
            "le retour arrière doit s'être déclenché tout seul"
        );
    }
}
