//! The setup wizard's requests (`/api/setup`, admins only): how cards are
//! recognised (switched while running), what a local Ollama server has, a
//! model download with progress, a one-card test of the chosen model, the
//! telemetry choice, and finishing.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use mtg_scan_shared::{
    BenchmarkView, CloudProvider, OllamaModelView, OllamaView, PullView, RecognitionChoice,
    RecognitionSetup, RecognitionView, SetupView, TelemetryView,
};
use serde::Deserialize;

use super::settings::lock;
use super::{AppState, blocking};
use crate::accounts::Signed;
use crate::config::{self, LlmConfig, LlmProvider, RecognitionMode, StorageBackend};
use crate::llm::VisionLlm;
use crate::ollama::{self, Ollama};
use crate::scryfall::Scryfall;
use crate::telemetry::{self, Facts};

/// The model suggested for Ollama (see CLAUDE.md: the instruct tag, not
/// the thinking one).
const LOCAL_MODEL: &str = "qwen3-vl:8b-instruct";
/// The card the test reads: Lightning Bolt, Masters 25 #141.
const TEST_CARD: &str = "e3285e6b-3e79-4d7c-bf96-d920f973b122";

/// A model download in progress (one at a time).
#[derive(Default)]
pub struct SetupState {
    pull: Arc<Mutex<PullView>>,
}

fn cloud_providers() -> Vec<CloudProvider> {
    [
        (
            LlmProvider::Anthropic,
            "Anthropic (Claude)",
            "https://console.anthropic.com/settings/keys",
        ),
        (
            LlmProvider::Gemini,
            "Google (Gemini)",
            "https://aistudio.google.com/apikey",
        ),
        (
            LlmProvider::Openrouter,
            "OpenRouter (many models)",
            "https://openrouter.ai/keys",
        ),
        (
            LlmProvider::Openai,
            "OpenAI",
            "https://platform.openai.com/api-keys",
        ),
    ]
    .into_iter()
    .map(|(p, label, key_url)| CloudProvider {
        id: p.as_str().into(),
        label: label.into(),
        default_model: LlmConfig {
            provider: p,
            ..LlmConfig::default()
        }
        .resolved_model(),
        key_url: key_url.into(),
    })
    .collect()
}

fn choice(mode: RecognitionMode, provider: LlmProvider) -> RecognitionChoice {
    match (mode, provider) {
        (RecognitionMode::Ocr, _) | (_, LlmProvider::None) => RecognitionChoice::Ocr,
        (_, p) if p.needs_key() => RecognitionChoice::Cloud,
        _ => RecognitionChoice::Local,
    }
}

pub async fn get(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
) -> Response {
    let state = st.clone();
    blocking(StatusCode::INTERNAL_SERVER_ERROR, move || {
        view(&state, &signed)
    })
    .await
}

fn view(st: &AppState, signed: &Signed) -> Result<SetupView> {
    let (_, prefs) = st.accounts.prefs(&signed.user, &st.cfg)?;
    let settings = lock(&st.settings);
    let llm = &settings.llm;
    let (storage, storage_location) = match st.cfg.storage.backend {
        StorageBackend::Postgres => (
            "PostgreSQL".to_string(),
            crate::app::App::for_user(&st.cfg, signed.user.id)
                .map(|app| app.store.location())
                .unwrap_or_else(|e| format!("{e:#}")),
        ),
        _ => (
            "SQLite".to_string(),
            std::path::absolute(st.cfg.storage.sqlite_path())
                .unwrap_or_else(|_| st.cfg.storage.sqlite_path())
                .display()
                .to_string(),
        ),
    };
    Ok(SetupView {
        done: settings.setup_done,
        storage,
        storage_location,
        recognition: RecognitionView {
            choice: choice(settings.recognition.mode, llm.provider),
            provider: llm.provider.as_str().into(),
            model: llm.resolved_model(),
            base_url: llm.resolved_base_url(),
            ocr: format!("{:?}", settings.recognition.ocr),
            own_key: prefs.llm_key.is_some(),
            server_key: llm.resolved_api_key().is_some(),
        },
        cloud_providers: cloud_providers(),
        local_model: LOCAL_MODEL.into(),
        telemetry: settings.telemetry,
    })
}

#[derive(Deserialize)]
pub struct OllamaQuery {
    url: Option<String>,
}

const OLLAMA_URL: &str = "http://localhost:11434";

/// What the Ollama server at `url` (default: the configured one) has.
pub async fn ollama(State(st): State<Arc<AppState>>, Query(q): Query<OllamaQuery>) -> Response {
    let configured = {
        let settings = lock(&st.settings);
        (settings.llm.provider == LlmProvider::Ollama)
            .then(|| settings.llm.resolved_base_url())
            .flatten()
    };
    let url = q
        .url
        .filter(|u| !u.trim().is_empty())
        .or(configured)
        .unwrap_or_else(|| OLLAMA_URL.into());
    blocking(StatusCode::BAD_REQUEST, move || Ok(probe(url.trim()))).await
}

fn probe(url: &str) -> OllamaView {
    let mut view = OllamaView {
        url: url.to_string(),
        version: None,
        error: None,
        models: Vec::new(),
        ready: false,
    };
    let checked = (|| -> Result<()> {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            bail!("the address starts with http:// or https://");
        }
        let server = Ollama::new(url)?;
        view.version = Some(server.version()?);
        view.models = server
            .installed()?
            .into_iter()
            .map(|m| OllamaModelView {
                name: m.name,
                size_gb: m.size as f64 / 1e9,
            })
            .collect();
        view.ready = server.show(LOCAL_MODEL)?.is_some_and(|info| info.vision());
        Ok(())
    })();
    if let Err(e) = checked {
        view.error = Some(format!("{e:#}"));
    }
    view
}

/// Switch how cards are recognised: saved in the settings file and used
/// from the next scan on, without a restart.
pub async fn recognition(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
    Json(setup): Json<RecognitionSetup>,
) -> Response {
    if st.sessions.dry_run() {
        return (StatusCode::CONFLICT, "Dry run: nothing can be changed.").into_response();
    }
    let state = st.clone();
    blocking(StatusCode::BAD_REQUEST, move || {
        apply_recognition(&state, &setup)?;
        view(&state, &signed)
    })
    .await
}

fn apply_recognition(st: &AppState, setup: &RecognitionSetup) -> Result<()> {
    let mut settings = lock(&st.settings);
    let mut llm = settings.llm.clone();
    let mut recognition = settings.recognition.clone();
    let model = Some(setup.model.trim().to_string()).filter(|m| !m.is_empty());
    match setup.choice {
        RecognitionChoice::Ocr => {
            recognition.mode = RecognitionMode::Ocr;
            llm.provider = LlmProvider::None;
            llm.model = None;
            llm.base_url = None;
        }
        RecognitionChoice::Local => {
            let url = setup.base_url.trim().trim_end_matches('/');
            if !url.is_empty() && !(url.starts_with("http://") || url.starts_with("https://")) {
                bail!("The Ollama address starts with http:// or https://");
            }
            recognition.mode = RecognitionMode::Auto;
            llm.provider = LlmProvider::Ollama;
            llm.model = model;
            llm.base_url = Some(url.to_string()).filter(|u| !u.is_empty() && u != OLLAMA_URL);
        }
        RecognitionChoice::Cloud => {
            let provider = LlmProvider::parse(&setup.provider)
                .filter(|p| p.needs_key())
                .ok_or_else(|| anyhow!("\"{}\" isn't a cloud provider", setup.provider))?;
            recognition.mode = RecognitionMode::Auto;
            llm.provider = provider;
            llm.model = model;
            llm.base_url = None;
        }
    }
    config::save_recognition(&settings.file, recognition.mode, &llm)?;
    st.sessions.reconfigure(recognition.clone(), llm.clone());
    settings.recognition = recognition;
    settings.llm = llm;
    Ok(())
}

#[derive(Deserialize)]
pub struct PullRequest {
    model: Option<String>,
}

/// Start downloading a model into the configured (or default) Ollama.
pub async fn start_pull(
    State(st): State<Arc<AppState>>,
    Json(req): Json<PullRequest>,
) -> Json<PullView> {
    let model = req
        .model
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| LOCAL_MODEL.into());
    let url = {
        let settings = lock(&st.settings);
        (settings.llm.provider == LlmProvider::Ollama)
            .then(|| settings.llm.resolved_base_url())
            .flatten()
            .unwrap_or_else(|| OLLAMA_URL.into())
    };
    let progress = st.setup.pull.clone();
    {
        let mut p = progress.lock().unwrap_or_else(|e| e.into_inner());
        if p.running {
            return Json(p.clone());
        }
        *p = PullView {
            model: model.clone(),
            status: "starting".into(),
            running: true,
            ..PullView::default()
        };
    }
    let shared = progress.clone();
    std::thread::spawn(move || {
        let update = |f: &dyn Fn(&mut PullView)| {
            f(&mut shared.lock().unwrap_or_else(|e| e.into_inner()));
        };
        let result = Ollama::new(&url).and_then(|server| {
            server.pull(&model, |p| {
                update(&|v: &mut PullView| {
                    v.status = p.status.clone();
                    v.percent = match (p.completed, p.total) {
                        (Some(done), Some(total)) if total > 0 => {
                            Some(done as f64 * 100.0 / total as f64)
                        }
                        _ => None,
                    };
                })
            })
        });
        update(&|v: &mut PullView| {
            v.running = false;
            match &result {
                Ok(()) => {
                    v.done = true;
                    v.status = "ready".into();
                    v.percent = Some(100.0);
                }
                Err(e) => v.error = Some(format!("{e:#}")),
            }
        });
    });
    Json(progress.lock().unwrap_or_else(|e| e.into_inner()).clone())
}

pub async fn pull(State(st): State<Arc<AppState>>) -> Json<PullView> {
    Json(
        st.setup
            .pull
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone(),
    )
}

/// Read one card with the chosen model and time it.
pub async fn benchmark(
    State(st): State<Arc<AppState>>,
    Extension(signed): Extension<Signed>,
) -> Response {
    let state = st.clone();
    blocking(StatusCode::BAD_REQUEST, move || {
        run_benchmark(&state, &signed)
    })
    .await
}

fn run_benchmark(st: &AppState, signed: &Signed) -> Result<BenchmarkView> {
    let llm = lock(&st.settings).llm.clone();
    let mut vision = VisionLlm::from_config(&llm)?
        .context("No vision model is chosen: cards are read from their printed text only")?;
    // The admin's own key first, as their scans do.
    let (_, prefs) = st.accounts.prefs(&signed.user, &st.cfg)?;
    if let (Some(secrets), Some(sealed)) = (&st.secrets, prefs.llm_key) {
        vision.set_user_key(Some(secrets.unseal(&sealed)?));
    }
    let http = crate::app::http_client()?;
    let card = Scryfall::new(http.clone())
        .by_id(TEST_CARD)?
        .context("Scryfall doesn't have the test card")?;
    let url = card
        .image_normal
        .clone()
        .or_else(|| card.image_small.clone())
        .context("the test card has no image")?;
    let bytes = http
        .get(&url)
        .send()
        .and_then(|r| r.error_for_status())
        .context("couldn't download the test card's image")?
        .bytes()?;
    let image = image::load_from_memory(&bytes).context("unreadable test image")?;

    let started = Instant::now();
    let read = vision.read_card(&image);
    let seconds = started.elapsed().as_secs_f64();
    let gpu_share = (llm.provider == LlmProvider::Ollama)
        .then(|| llm.resolved_base_url())
        .flatten()
        .and_then(|url| Ollama::new(&url).ok()?.loaded().ok())
        .and_then(|loaded| {
            loaded
                .into_iter()
                .find(|l| ollama::same_model(&l.name, &llm.resolved_model()))
                .map(|l| l.on_gpu())
        });
    let (read, error) = match read {
        Ok((hints, _)) => (hints.name, None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    let correct = read
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case(&card.name));
    let verdict = match (&error, seconds) {
        (Some(_), _) => "It didn't work: see the message.".to_string(),
        (None, s) if s < 6.0 => "Fast: good for scanning.".to_string(),
        (None, s) if s < 20.0 => {
            "Fine: each card the printed text can't settle takes this long.".to_string()
        }
        _ => "Slow: consider a smaller model, a cloud model, or the printed text only.".to_string(),
    };
    Ok(BenchmarkView {
        seconds,
        expected: card.name,
        read,
        correct,
        gpu_share,
        verdict,
        error,
    })
}

#[derive(Deserialize)]
pub struct TelemetryChoice {
    enabled: bool,
}

/// How the server is set up, for the usage statistics. With `when_allowed`,
/// `None` while sending isn't allowed.
pub fn telemetry_facts(st: &AppState, when_allowed: bool) -> Option<Facts> {
    let facts = {
        let settings = lock(&st.settings);
        if when_allowed && !telemetry::allowed(settings.telemetry) {
            return None;
        }
        Facts::new(
            st.cfg.storage.backend,
            &settings.recognition,
            &settings.llm,
            settings.card_kingdom,
            settings.setup_done,
            0,
        )
    };
    let accounts = st
        .accounts
        .users()
        .map(|users| users.iter().filter(|u| u.has_password).count() as u64)
        .unwrap_or(1);
    Some(Facts { accounts, ..facts })
}

/// The choice, and exactly what the next ping sends.
pub async fn telemetry_view(State(st): State<Arc<AppState>>) -> Response {
    let state = st.clone();
    blocking(StatusCode::INTERNAL_SERVER_ERROR, move || {
        let facts = telemetry_facts(&state, false).context("no settings")?;
        let mut log = telemetry::Log::load(&telemetry::state_file());
        let next = log.preview(&facts)?;
        // Keeps the id it showed.
        log.save()?;
        Ok(TelemetryView {
            enabled: lock(&state.settings).telemetry,
            do_not_track: telemetry::do_not_track(),
            url: telemetry::url(&state.cfg.telemetry),
            last_sent: log.last_sent().map(str::to_string),
            next,
        })
    })
    .await
}

pub async fn telemetry(
    State(st): State<Arc<AppState>>,
    Json(choice): Json<TelemetryChoice>,
) -> Response {
    let state = st.clone();
    blocking(StatusCode::BAD_REQUEST, move || {
        let mut settings = lock(&state.settings);
        config::save_telemetry(&settings.file, choice.enabled)?;
        settings.telemetry = Some(choice.enabled);
        Ok(())
    })
    .await
}

#[derive(Deserialize)]
pub struct DoneChoice {
    #[serde(default = "yes")]
    done: bool,
}

fn yes() -> bool {
    true
}

/// Finish (or skip) the wizard; `done: false` shows it again.
pub async fn done(State(st): State<Arc<AppState>>, Json(choice): Json<DoneChoice>) -> Response {
    let state = st.clone();
    blocking(StatusCode::BAD_REQUEST, move || {
        let mut settings = lock(&state.settings);
        config::save_setup_done(&settings.file, choice.done)?;
        settings.setup_done = choice.done;
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choice_follows_mode_and_provider() {
        assert_eq!(
            choice(RecognitionMode::Auto, LlmProvider::None),
            RecognitionChoice::Ocr
        );
        assert_eq!(
            choice(RecognitionMode::Ocr, LlmProvider::Ollama),
            RecognitionChoice::Ocr
        );
        assert_eq!(
            choice(RecognitionMode::Auto, LlmProvider::Ollama),
            RecognitionChoice::Local
        );
        assert_eq!(
            choice(RecognitionMode::Auto, LlmProvider::Anthropic),
            RecognitionChoice::Cloud
        );
        let providers = cloud_providers();
        assert!(providers.iter().all(|p| !p.default_model.is_empty()));
        assert!(
            providers
                .iter()
                .any(|p| p.id == "anthropic" && p.default_model == "claude-opus-5-5")
        );
    }

    #[test]
    fn a_bad_ollama_address_is_explained() {
        let view = probe("localhost:11434");
        assert!(view.error.unwrap().contains("http://"));
        assert!(!view.ready);
    }
}
