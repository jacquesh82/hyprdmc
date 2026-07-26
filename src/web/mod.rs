//! Interface web : API REST + page unique servie depuis le binaire.
//!
//! Les fichiers de `assets/` sont embarqués à la compilation : `hyprmc` reste
//! un exécutable unique, sans chaîne de compilation JavaScript ni fichiers à
//! installer à côté.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response, Sse, sse::Event};
use axum::routing::{delete, get, post, put};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::json;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::config::{OutputRule, Profile};
use crate::daemon::AppState;
use crate::emit;
use crate::layout::{Layout, OutputState};

#[derive(RustEmbed)]
#[folder = "src/web/assets/"]
struct Assets;

/// Enveloppe d'erreur : toute erreur métier ressort en JSON exploitable par
/// l'interface.
#[derive(Debug)]
struct ApiError(anyhow::Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::warn!("erreur API : {:#}", self.0);
        (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({ "error": format!("{:#}", self.0) })),
        )
            .into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

type ApiResult<T> = std::result::Result<T, ApiError>;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/state", get(get_state))
        .route("/api/monitors", get(get_monitors))
        .route("/api/apply", post(post_apply))
        .route("/api/confirm", post(post_confirm))
        .route("/api/revert", post(post_revert))
        .route("/api/persist", post(post_persist))
        .route("/api/profiles", get(get_profiles))
        .route("/api/profiles/{name}", put(put_profile))
        .route("/api/profiles/{name}", delete(delete_profile))
        .route("/api/profiles/{name}/apply", post(apply_profile))
        .route("/api/events", get(sse_events))
        .fallback(static_handler)
        .with_state(state)
}

/// Démarre le serveur. Ne rend la main qu'à l'arrêt du serveur.
pub async fn serve(state: Arc<AppState>, addr: SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("écoute sur {addr} impossible"))?;
    tracing::info!("interface web : http://{addr}");
    axum::serve(listener, router(state))
        .await
        .context("le serveur web s'est arrêté")
}

async fn get_state(State(state): State<Arc<AppState>>) -> ApiResult<axum::Json<serde_json::Value>> {
    Ok(axum::Json(state.state_json().await?))
}

async fn get_monitors(
    State(state): State<Arc<AppState>>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    Ok(axum::Json(json!(state.monitors().await?)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApplyRequest {
    outputs: Vec<OutputState>,
    #[serde(default)]
    force: bool,
    /// Armer le retour arrière automatique (vrai par défaut depuis l'UI).
    #[serde(default = "yes")]
    guard: bool,
}

fn yes() -> bool {
    true
}

async fn post_apply(
    State(state): State<Arc<AppState>>,
    axum::Json(req): axum::Json<ApplyRequest>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    let layout = Layout::new(req.outputs);
    let report = state.apply(layout, req.force, req.guard).await?;
    Ok(axum::Json(json!(report)))
}

async fn post_confirm(
    State(state): State<Arc<AppState>>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    let confirmed = state.confirm().await;
    state.broadcast().await;
    Ok(axum::Json(json!({ "confirmed": confirmed })))
}

async fn post_revert(
    State(state): State<Arc<AppState>>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    Ok(axum::Json(json!({ "reverted": state.revert_now().await? })))
}

async fn post_persist(
    State(state): State<Arc<AppState>>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    let layout = Layout::from_monitors(&state.monitors().await?);
    let path = state.config.read().await.settings.monitors_conf.clone();
    emit::persist(&layout, &path)?;
    Ok(axum::Json(json!({ "path": path })))
}

async fn get_profiles(
    State(state): State<Arc<AppState>>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    let monitors = state.monitors().await?;
    let cfg = state.config.read().await;
    Ok(axum::Json(json!({
        "profiles": cfg.profiles,
        "activeProfile": cfg.best_match(&monitors).map(|p| p.name.clone()),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveProfileRequest {
    /// Agencement à enregistrer. Absent = état live courant.
    #[serde(default)]
    outputs: Option<Vec<OutputState>>,
    #[serde(default)]
    exact: bool,
}

async fn put_profile(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    axum::Json(req): axum::Json<SaveProfileRequest>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    let monitors = state.monitors().await?;
    let outputs = req
        .outputs
        .unwrap_or_else(|| Layout::from_monitors(&monitors).outputs);

    let rules = outputs
        .iter()
        .map(|o| OutputRule::from_state(o, monitors.iter().find(|m| m.name == o.name)))
        .collect();

    let profile = Profile {
        name: name.clone(),
        exact: req.exact,
        outputs: rules,
    };

    let mut cfg = state.config.write().await;
    cfg.upsert(profile);
    let path = cfg.save()?;
    drop(cfg);

    state.broadcast().await;
    Ok(axum::Json(json!({ "saved": name, "path": path })))
}

async fn delete_profile(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    let mut cfg = state.config.write().await;
    cfg.remove(&name)?;
    cfg.save()?;
    drop(cfg);

    state.broadcast().await;
    Ok(axum::Json(json!({ "deleted": name })))
}

async fn apply_profile(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    let monitors = state.monitors().await?;
    let layout = {
        let cfg = state.config.read().await;
        let profile = cfg
            .profile(&name)
            .ok_or_else(|| anyhow::anyhow!("profil « {name} » inconnu"))?;
        profile.resolve(&monitors)?
    };
    let report = state.apply(layout, false, true).await?;
    Ok(axum::Json(json!(report)))
}

/// Flux d'état poussé aux clients : hotplug, application, retour arrière.
async fn sse_events(
    State(state): State<Arc<AppState>>,
) -> Sse<impl tokio_stream::Stream<Item = std::result::Result<Event, std::convert::Infallible>>> {
    let rx = state.events.subscribe();

    // Premier message : l'état courant, pour que le client parte du bon pied.
    let initial = state
        .state_json()
        .await
        .map(|v| v.to_string())
        .unwrap_or_else(|e| json!({ "error": e.to_string() }).to_string());

    let stream = tokio_stream::once(initial)
        .chain(BroadcastStream::new(rx).filter_map(std::result::Result::ok))
        .map(|payload| Ok(Event::default().data(payload)));

    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path) {
        Some(file) => {
            let mime = file.metadata.mimetype().to_string();
            ([(header::CONTENT_TYPE, mime)], file.data.into_owned()).into_response()
        }
        None => (StatusCode::NOT_FOUND, "introuvable").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::ipc::fake::FakeBackend;

    const MONITORS: &str = r#"[
      {"id":0,"name":"eDP-1","description":"AU Optronics 0x5799","make":"AU Optronics",
       "model":"0x5799","serial":"","width":1920,"height":1080,"refreshRate":60.0,
       "x":0,"y":0,"scale":1.0,"transform":0,"focused":true,"disabled":false,
       "mirrorOf":"none","vrr":false,"availableModes":["1920x1080@60.00Hz"]}
    ]"#;

    fn state() -> Arc<AppState> {
        AppState::new(
            Arc::new(FakeBackend::with_monitors(MONITORS)),
            Config::default(),
        )
    }

    #[tokio::test]
    async fn state_endpoint_returns_the_live_layout() {
        let json = get_state(State(state())).await.unwrap().0;
        assert_eq!(json["monitors"][0]["name"], "eDP-1");
        assert_eq!(json["layout"]["outputs"][0]["name"], "eDP-1");
    }

    #[tokio::test]
    async fn apply_endpoint_round_trips_an_output_state() {
        let state = state();
        let outputs = Layout::from_monitors(&state.monitors().await.unwrap()).outputs;
        let req = ApplyRequest {
            outputs,
            force: false,
            guard: false,
        };
        let json = post_apply(State(Arc::clone(&state)), axum::Json(req))
            .await
            .unwrap()
            .0;
        assert_eq!(json["specs"].as_array().unwrap().len(), 1);
        assert_eq!(json["rolled_back"], false);
    }

    #[tokio::test]
    async fn applying_an_unknown_profile_is_rejected() {
        let err = apply_profile(State(state()), Path("absent".to_string()))
            .await
            .expect_err("un profil inconnu doit échouer");
        assert!(format!("{:#}", err.0).contains("inconnu"));
    }

    #[tokio::test]
    async fn assets_are_embedded_in_the_binary() {
        assert!(
            Assets::get("index.html").is_some(),
            "la page doit être embarquée"
        );
        assert!(Assets::get("app.js").is_some());
        assert!(Assets::get("style.css").is_some());
    }

    #[tokio::test]
    async fn unknown_asset_yields_404() {
        let resp = static_handler("/absent.js".parse::<Uri>().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
