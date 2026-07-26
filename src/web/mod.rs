//! Web interface: REST API + single page served from the binary.
//!
//! The files under `assets/` are embedded at compile time: `hyprdmc` stays a
//! single executable, with no JavaScript build chain and no files to install
//! alongside it.

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

/// Error envelope: any business-logic error comes back as JSON the UI can use.
#[derive(Debug)]
struct ApiError(anyhow::Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::warn!("API error: {:#}", self.0);
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
        .route("/api/history", get(get_history))
        .route("/api/history/{index}/restore", post(restore_history))
        .route("/api/events", get(sse_events))
        .route("/api/i18n", get(get_i18n))
        .fallback(static_handler)
        .with_state(state)
}

/// Claims the listening socket.
///
/// Separate from [`serve_on`] so that callers can know the port is accepting
/// connections *before* they act on it — pointing a browser at a socket that
/// is not bound yet is a race worth avoiding.
pub async fn bind(addr: SocketAddr) -> Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("could not listen on {addr}"))
}

/// Serves the UI on an already-bound listener. Only returns on shutdown.
pub async fn serve_on(listener: tokio::net::TcpListener, state: Arc<AppState>) -> Result<()> {
    axum::serve(listener, router(state))
        .await
        .context("the web server stopped")
}

/// Binds and serves in one step.
pub async fn serve(state: Arc<AppState>, addr: SocketAddr) -> Result<()> {
    let listener = bind(addr).await?;
    tracing::info!("web interface: http://{addr}");
    serve_on(listener, state).await
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
    /// Arm the automatic revert (true by default from the UI).
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
    /// Layout to save. Absent = current live state.
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
        let profile = cfg.profile(&name).ok_or_else(|| {
            anyhow::anyhow!(rust_i18n::t!("config.unknown_profile", name = name).to_string())
        })?;
        profile.resolve(&monitors)?
    };
    let report = state.apply(layout, false, true).await?;
    Ok(axum::Json(json!(report)))
}

async fn get_history(
    State(state): State<Arc<AppState>>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    let store = state.store.lock().await;
    // The age is computed server-side so the UI needs no clock of its own and
    // cannot drift from what the CLI shows.
    let entries: Vec<serde_json::Value> = store
        .history
        .iter()
        .enumerate()
        .map(|(index, s)| {
            json!({
                "index": index,
                "when": s.age_label(),
                "profile": s.profile,
                "summary": s.describe(),
                "layout": s.layout,
            })
        })
        .collect();
    Ok(axum::Json(
        json!({ "entries": entries, "remembered": store.recall.len() }),
    ))
}

async fn restore_history(
    State(state): State<Arc<AppState>>,
    Path(index): Path<usize>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    let snapshot = state.restore(index).await?;
    Ok(axum::Json(json!({
        "restored": index,
        "when": snapshot.age_label(),
    })))
}

/// State stream pushed to clients: hotplug, apply, revert.
async fn sse_events(
    State(state): State<Arc<AppState>>,
) -> Sse<impl tokio_stream::Stream<Item = std::result::Result<Event, std::convert::Infallible>>> {
    let rx = state.events.subscribe();

    // First message: the current state, so the client starts on the right foot.
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

/// Keys under the `web.` prefix of `locales/app.yml`, exposed to the browser
/// by `/api/i18n`.
///
/// rust-i18n has no API to list keys by prefix, so this list is kept explicit
/// and must stay in sync with the `web.*` section of `locales/app.yml`.
const WEB_KEYS: &[&str] = &[
    "web.title",
    "web.profile_badge",
    "web.connection",
    "web.canvas_label",
    "web.hint",
    "web.select_prompt",
    "web.guard.applied",
    "web.guard.keep",
    "web.guard.revert",
    "web.action.apply",
    "web.action.reset",
    "web.action.auto",
    "web.action.save",
    "web.action.persist",
    "web.field.enabled",
    "web.field.mode",
    "web.field.scale",
    "web.field.rotation",
    "web.field.flip",
    "web.field.mirror",
    "web.field.vrr",
    "web.mirror.none",
    "web.screen.disabled",
    "web.screen.flipped",
    "web.prompt.profile_name",
    "web.toast.applied",
    "web.toast.rolled_back",
    "web.toast.kept",
    "web.toast.reverted",
    "web.toast.profile_saved",
    "web.toast.persisted",
    "web.issue.overlap",
    "web.issue.all_disabled",
    "web.issue.mirror_unavailable",
    "web.not_found",
    "web.theme.toggle_label",
    "web.theme.auto",
    "web.theme.light",
    "web.theme.dark",
    "web.history.title",
    "web.history.empty",
    "web.history.remembered",
    "web.history.origin_manual",
    "web.history.restore",
    "web.history.restore_aria",
    "web.history.restored",
    "web.no_outputs",
    "web.disconnected",
];

/// Serves the strings the UI needs for the active locale.
///
/// Values keep their `%{name}` placeholders untouched (no `t!` arguments are
/// passed): substitution happens client-side, in the JS `t()` helper.
async fn get_i18n() -> axum::Json<serde_json::Value> {
    let strings: serde_json::Map<String, serde_json::Value> = WEB_KEYS
        .iter()
        .map(|&key| (key.to_string(), json!(rust_i18n::t!(key).into_owned())))
        .collect();
    axum::Json(json!({
        "locale": crate::i18n::current(),
        "strings": strings,
    }))
}

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path) {
        Some(file) => {
            let mime = file.metadata.mimetype().to_string();
            ([(header::CONTENT_TYPE, mime)], file.data.into_owned()).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            rust_i18n::t!("web.not_found").into_owned(),
        )
            .into_response(),
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
            .expect_err("an unknown profile must fail");
        assert!(format!("{:#}", err.0).contains("unknown profile"));
    }

    /// The locale file, read at compile time so the checks below cannot drift
    /// from what actually ships.
    const LOCALES: &str = include_str!("../../locales/app.yml");

    #[test]
    fn every_served_key_has_a_translation() {
        // rust-i18n echoes the key back when it is missing, which would ship a
        // raw `web.action.apply` to the UI instead of a label.
        for key in WEB_KEYS {
            for locale in crate::i18n::AVAILABLE {
                let value = rust_i18n::t!(*key, locale = *locale);
                assert_ne!(
                    value, *key,
                    "key {key} has no {locale} translation in locales/app.yml"
                );
            }
        }
    }

    #[test]
    fn every_web_key_in_the_locale_file_is_actually_served() {
        // Adding a key to locales/app.yml is not enough: /api/i18n only sends
        // what WEB_KEYS lists, so a forgotten entry silently stays English.
        let declared: Vec<&str> = LOCALES
            .lines()
            .filter_map(|l| l.strip_suffix(':'))
            .filter(|k| k.starts_with("web."))
            .collect();

        assert!(
            !declared.is_empty(),
            "no web.* key found in the locale file"
        );
        for key in declared {
            assert!(
                WEB_KEYS.contains(&key),
                "{key} is translated but missing from WEB_KEYS, so the UI never receives it"
            );
        }
    }

    #[tokio::test]
    async fn assets_are_embedded_in_the_binary() {
        assert!(
            Assets::get("index.html").is_some(),
            "the page must be embedded"
        );
        assert!(Assets::get("app.js").is_some());
        assert!(Assets::get("style.css").is_some());
    }

    #[tokio::test]
    async fn unknown_asset_yields_404() {
        let resp = static_handler("/absent.js".parse::<Uri>().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn i18n_endpoint_returns_the_active_locale_and_known_strings() {
        let json = get_i18n().await.0;
        assert!(json["locale"].is_string());
        let strings = json["strings"].as_object().unwrap();
        assert!(!strings.is_empty());
        assert_eq!(strings["web.action.apply"], "Apply");
    }
}
