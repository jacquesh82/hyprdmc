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
        .route("/api/input", get(get_input).put(put_input))
        .route("/api/input/persist", post(persist_input))
        .route("/api/config", get(export_config).post(import_config))
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
    // No profile: the web UI edits a layout by hand. Saving it under a name is
    // a separate, deliberate action (PUT /api/profiles/{name}).
    let report = state.apply(layout, req.force, req.guard, None).await?;
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
    let path = state.config.read().await.settings.monitors_lua.clone();
    emit::persist(&layout, &path)?;
    Ok(axum::Json(json!({ "path": path })))
}

// ----------------------------------------------------------- import/export --

/// Marker carried by an exported file, so an import can tell a hyprdmc
/// configuration from any other JSON the user happened to pick.
const BUNDLE_KIND: &str = "hyprdmc-config";
/// Bumped when the shape changes in a way an older hyprdmc cannot read.
const BUNDLE_VERSION: u32 = 1;

/// What travels in an exported file.
#[derive(Debug, serde::Serialize, Deserialize)]
struct ConfigBundle {
    kind: String,
    version: u32,
    config: crate::config::Config,
}

/// The whole configuration, as a file to keep.
async fn export_config(
    State(state): State<Arc<AppState>>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    let config = state.config.read().await.clone();
    Ok(axum::Json(json!(ConfigBundle {
        kind: BUNDLE_KIND.to_string(),
        version: BUNDLE_VERSION,
        config,
    })))
}

/// Replaces the configuration with an exported one.
///
/// Machine-local plumbing is deliberately *not* imported: the listening port,
/// the bind address and the two generated-file paths stay as they are here. A
/// configuration exported on another machine carries that machine's home
/// directory, and silently writing `monitors.lua` into a path that does not
/// exist on this one is a bug waiting to be reported as "hyprdmc stopped
/// working". Everything the user actually meant to move — profiles, keyboard
/// and pointer, behaviour — comes across.
async fn import_config(
    State(state): State<Arc<AppState>>,
    axum::Json(bundle): axum::Json<ConfigBundle>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    if bundle.kind != BUNDLE_KIND {
        return Err(anyhow::anyhow!(rust_i18n::t!("config.not_a_bundle").to_string()).into());
    }
    if bundle.version > BUNDLE_VERSION {
        return Err(anyhow::anyhow!(
            rust_i18n::t!(
                "config.bundle_too_new",
                version = bundle.version,
                supported = BUNDLE_VERSION
            )
            .to_string()
        )
        .into());
    }

    let mut cfg = state.config.write().await;
    let local = cfg.settings.clone();
    *cfg = bundle.config;
    cfg.settings.web_port = local.web_port;
    cfg.settings.bind = local.bind;
    cfg.settings.monitors_lua = local.monitors_lua;
    cfg.settings.input_lua = local.input_lua;
    let path = cfg.save()?;
    let profiles = cfg.profiles.len();
    drop(cfg);

    state.broadcast().await;
    Ok(axum::Json(json!({ "profiles": profiles, "path": path })))
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
    let report = state.apply(layout, false, true, Some(name)).await?;
    Ok(axum::Json(json!(report)))
}

// ------------------------------------------------------------------ input --

/// Keyboard and pointer settings, plus the catalogue the UI needs to offer a
/// choice at all.
///
/// The values come from the compositor rather than from `config.toml`: what is
/// live is the truth, and a layout set by hand in `hyprland.lua` must show up
/// here instead of being silently overwritten by a default.
async fn get_input(State(state): State<Arc<AppState>>) -> ApiResult<axum::Json<serde_json::Value>> {
    let backend = Arc::clone(&state.backend);
    let current =
        tokio::task::spawn_blocking(move || crate::input::InputConfig::read(backend.as_ref()))
            .await
            .map_err(anyhow::Error::from)??;
    // Parsing the xkb rules means reading a ~100 kB file: off the executor,
    // like every other blocking call here.
    let catalog = tokio::task::spawn_blocking(crate::input::catalog)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(axum::Json(json!({
        "current": current,
        "catalog": catalog,
        "path": state.config.read().await.settings.input_lua,
    })))
}

/// Applies the settings and records them in `config.toml`.
///
/// No revert guard here, unlike the screen layout: a keyboard layout you
/// cannot type in is annoying, not a lock-out — the mouse still works, and the
/// UI is still readable. The countdown is for changes that can leave you
/// staring at a black screen.
async fn put_input(
    State(state): State<Arc<AppState>>,
    axum::Json(input): axum::Json<crate::input::InputConfig>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    input.validate()?;

    let backend = Arc::clone(&state.backend);
    let target = input.clone();
    tokio::task::spawn_blocking(move || target.apply(backend.as_ref()))
        .await
        .map_err(anyhow::Error::from)??;

    let mut cfg = state.config.write().await;
    cfg.input = input.clone();
    cfg.save()?;
    Ok(axum::Json(json!({ "applied": input })))
}

/// Writes `input.lua`, so the settings survive a compositor restart.
async fn persist_input(
    State(state): State<Arc<AppState>>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    let backend = Arc::clone(&state.backend);
    // The live state again, not `config.toml`: what gets written is what the
    // user is currently using.
    let current =
        tokio::task::spawn_blocking(move || crate::input::InputConfig::read(backend.as_ref()))
            .await
            .map_err(anyhow::Error::from)??;
    let path = state.config.read().await.settings.input_lua.clone();
    emit::persist_input(&current, &path)?;
    Ok(axum::Json(json!({ "path": path })))
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
    "web.guard.countdown",
    "web.guard.aria",
    "web.guard.keep",
    "web.guard.revert",
    "web.guard.keys",
    "web.action.apply",
    "web.action.apply_title",
    "web.action.pending",
    "web.action.reset",
    "web.action.auto",
    "web.action.rescan",
    "web.action.export",
    "web.action.export_title",
    "web.action.import",
    "web.action.import_title",
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
    "web.toast.rescan_found",
    "web.toast.rescan_none",
    "web.toast.exported",
    "web.toast.imported",
    "web.toast.import_unreadable",
    "web.import.confirm",
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
    "web.history.close",
    "web.no_outputs",
    "web.disconnected",
    "web.tabs_label",
    "web.tab.screens",
    "web.tab.input",
    "web.input.keyboard",
    "web.input.layout",
    "web.input.variant",
    "web.input.variant_none",
    "web.input.variant_help",
    "web.input.options",
    "web.input.options_add",
    "web.input.options_none",
    "web.input.option_remove",
    "web.input.pointer",
    "web.input.touchpad",
    "web.input.mouse",
    "web.input.scroll",
    "web.input.scroll_normal",
    "web.input.scroll_inverted",
    "web.input.scroll_help",
    "web.input.note",
    "web.toast.input_applied",
    "web.toast.input_persisted",
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

/// Serves an embedded asset, with revalidation.
///
/// These files are baked into the binary, so they change exactly when the
/// binary is rebuilt — and a browser that heuristically caches them keeps
/// running yesterday's UI against today's daemon, which looks like a bug in
/// the daemon. `no-cache` forces a revalidation on every load; the content
/// hash as `ETag` makes that revalidation cheap by answering `304` while
/// nothing has changed.
async fn static_handler(headers: axum::http::HeaderMap, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    let Some(file) = Assets::get(path) else {
        return (
            StatusCode::NOT_FOUND,
            rust_i18n::t!("web.not_found").into_owned(),
        )
            .into_response();
    };

    let etag = etag_for(&file.metadata.sha256_hash());
    let unchanged = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|sent| sent.split(',').any(|candidate| candidate.trim() == etag));

    let common = [
        (header::CACHE_CONTROL, "no-cache".to_string()),
        (header::ETAG, etag),
    ];

    if unchanged {
        return (StatusCode::NOT_MODIFIED, common).into_response();
    }

    let mime = file.metadata.mimetype().to_string();
    (
        common,
        [(header::CONTENT_TYPE, mime)],
        file.data.into_owned(),
    )
        .into_response()
}

/// Renders a content hash as a quoted entity tag.
fn etag_for(hash: &[u8; 32]) -> String {
    let mut tag = String::with_capacity(2 + 32);
    tag.push('"');
    // 8 bytes are ample to tell two builds apart and keep the header short.
    for byte in &hash[..8] {
        tag.push_str(&format!("{byte:02x}"));
    }
    tag.push('"');
    tag
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::ipc::fake::FakeBackend;
    use axum::http::HeaderMap;

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
    async fn assets_are_revalidated_rather_than_cached_blindly() {
        // A browser holding yesterday's app.js against today's daemon looks
        // exactly like a broken daemon, so these must never be cached without
        // a revalidation.
        let resp = static_handler(HeaderMap::new(), "/app.js".parse::<Uri>().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache"
        );
        assert!(resp.headers().get(header::ETAG).is_some());
    }

    #[tokio::test]
    async fn an_unchanged_asset_answers_304() {
        let uri = "/app.js".parse::<Uri>().unwrap();
        let first = static_handler(HeaderMap::new(), uri.clone()).await;
        let etag = first.headers().get(header::ETAG).unwrap().clone();

        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, etag);
        let second = static_handler(headers, uri).await;
        assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn a_stale_etag_gets_the_new_body() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::IF_NONE_MATCH,
            "\"0000000000000000\"".parse().unwrap(),
        );
        let resp = static_handler(headers, "/app.js".parse::<Uri>().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn different_assets_get_different_etags() {
        let js = static_handler(HeaderMap::new(), "/app.js".parse::<Uri>().unwrap()).await;
        let css = static_handler(HeaderMap::new(), "/style.css".parse::<Uri>().unwrap()).await;
        assert_ne!(
            js.headers().get(header::ETAG),
            css.headers().get(header::ETAG)
        );
    }

    #[tokio::test]
    async fn unknown_asset_yields_404() {
        let resp = static_handler(HeaderMap::new(), "/absent.js".parse::<Uri>().unwrap()).await;
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
