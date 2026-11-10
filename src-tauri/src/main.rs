// Pas de console supplémentaire sous Windows en mode release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bubble;
mod config;
mod langtool;
mod selection;

use config::Config;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

/// Arguments de ligne de commande (utiles pour lier un raccourci sous Wayland).
const ARGS_CHECK: &[&str] = &["--corriger", "--check"];
const ARGS_HIDDEN: &[&str] = &["--cache", "--hidden"];
const ARGS_SETTINGS: &[&str] = &["--parametres", "--settings"];

struct AppState {
    config: Mutex<Config>,
    config_path: PathBuf,
    http: reqwest::Client,
    pending: Mutex<Option<Capture>>,
    target: Mutex<selection::Target>,
    capturing: AtomicBool,
    capture_seq: AtomicU64,
    shortcut_error: Mutex<Option<String>>,
    shortcut_notice: Mutex<Option<String>>,
    /// Canal vers le thread de la bulle (Windows).
    bubble_tx: Mutex<Option<std::sync::mpsc::Sender<bubble::Command>>>,
    bubble_state: Mutex<Option<bubble::BubbleState>>,
    /// Le texte capturé est le champ entier (bulle) : on sélectionne tout avant de coller.
    replace_field: AtomicBool,
}

#[derive(Clone, Serialize)]
struct Capture {
    id: u64,
    text: String,
    from_selection: bool,
    /// "selection" (raccourci) ou "field" (champ entier, depuis la bulle)
    mode: &'static str,
    notice: Option<String>,
}

#[derive(Serialize)]
struct PlatformInfo {
    os: &'static str,
    wayland: bool,
    shortcut_active: bool,
    shortcut_error: Option<String>,
    shortcut_notice: Option<String>,
    bubble_supported: bool,
    config_path: String,
    version: &'static str,
}

#[derive(Serialize)]
struct ApplyResult {
    pasted: bool,
    message: Option<String>,
}

// ---------------------------------------------------------------------------
// Fenêtre
// ---------------------------------------------------------------------------

fn show_main(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.show();
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn hide_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

// ---------------------------------------------------------------------------
// Capture de la sélection
// ---------------------------------------------------------------------------

fn trigger_capture(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.capturing.swap(true, Ordering::SeqCst) {
        return; // une capture est déjà en cours
    }
    // Raccourci pressé alors que Coquille est déjà au premier plan : on ne capture rien
    // (on copierait notre propre texte), on se contente de garder la fenêtre.
    if let Some(w) = app.get_webview_window("main") {
        if w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false) {
            state.capturing.store(false, Ordering::SeqCst);
            return;
        }
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let target = selection::Target::current();
        let result = selection::capture();
        let id = state.capture_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let capture = match result {
            Ok(Some(text)) if !text.trim().is_empty() => {
                Capture { id, text, from_selection: true, mode: "selection", notice: None }
            }
            Ok(_) => Capture {
                id,
                text: String::new(),
                from_selection: false,
                mode: "selection",
                notice: Some(selection::NO_SELECTION.into()),
            },
            Err(e) => Capture { id, text: String::new(), from_selection: false, mode: "selection", notice: Some(e) },
        };
        state.replace_field.store(false, Ordering::SeqCst);
        *state.target.lock().unwrap() = target;
        *state.pending.lock().unwrap() = Some(capture.clone());
        show_main(&app);
        let _ = app.emit_to("main", "capture", capture);
        state.capturing.store(false, Ordering::SeqCst);
    });
}

/// Ouvre l'éditeur avec tout le texte d'un champ (depuis la bulle) ; « Remplacer » remplacera le champ entier.
#[allow(dead_code)]
pub(crate) fn open_editor_with_field(app: &AppHandle, text: String, hwnd: isize) {
    let state = app.state::<AppState>();
    let id = state.capture_seq.fetch_add(1, Ordering::SeqCst) + 1;
    let capture = Capture { id, text, from_selection: true, mode: "field", notice: None };
    #[cfg(windows)]
    {
        *state.target.lock().unwrap() = selection::Target::from_hwnd(hwnd);
    }
    #[cfg(not(windows))]
    let _ = hwnd;
    state.replace_field.store(true, Ordering::SeqCst);
    *state.pending.lock().unwrap() = Some(capture.clone());
    show_main(app);
    let _ = app.emit_to("main", "capture", capture);
}

fn handle_args(app: &AppHandle, argv: &[String], first_launch: bool) {
    let has = |names: &[&str]| argv.iter().skip(1).any(|a| names.contains(&a.as_str()));
    if has(ARGS_CHECK) {
        trigger_capture(app);
    } else if has(ARGS_SETTINGS) {
        show_main(app);
        let _ = app.emit_to("main", "open-settings", ());
    } else if !first_launch || !has(ARGS_HIDDEN) {
        show_main(app);
    }
}

// ---------------------------------------------------------------------------
// Raccourci global
// ---------------------------------------------------------------------------

fn register_shortcut(app: &AppHandle, accelerator: &str) -> Result<(), String> {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    gs.register(accelerator).map_err(|e| {
        let detail = e.to_string();
        if detail.to_lowercase().contains("already registered") {
            format!(
                "« {accelerator} » est déjà réservé par une autre application (lanceur, PowerToys, logiciel de clavier…). \
                 Choisis une autre combinaison dans ⚙ › Raccourci global."
            )
        } else {
            format!("« {accelerator} » n'est pas une combinaison valide ({detail}).")
        }
    })
}

/// Enregistre le raccourci configuré. Si c'est le raccourci par défaut et qu'il est déjà pris
/// par une autre application, essaie des combinaisons de repli et enregistre celle qui marche.
fn register_configured_shortcut(app: &AppHandle) {
    let state = app.state::<AppState>();
    let wanted = state.config.lock().unwrap().shortcut.clone();
    match register_shortcut(app, &wanted) {
        Ok(()) => {
            *state.shortcut_error.lock().unwrap() = None;
        }
        Err(e) if wanted == config::default_shortcut() => {
            let fallback = config::fallback_shortcuts()
                .into_iter()
                .find(|candidate| register_shortcut(app, candidate).is_ok());
            match fallback {
                Some(candidate) => {
                    let mut cfg = state.config.lock().unwrap().clone();
                    cfg.shortcut = candidate.clone();
                    let _ = persist(&state, cfg);
                    *state.shortcut_error.lock().unwrap() = None;
                    *state.shortcut_notice.lock().unwrap() = Some(format!(
                        "{wanted} était déjà pris par une autre application : Coquille utilise maintenant {candidate}. \
                         Tu peux le changer dans ⚙ › Raccourci global."
                    ));
                }
                None => *state.shortcut_error.lock().unwrap() = Some(e),
            }
        }
        Err(e) => *state.shortcut_error.lock().unwrap() = Some(e),
    }
}

fn shortcut_supported() -> bool {
    // Sous Wayland, une application ne peut pas intercepter un raccourci global :
    // il faut lier `coquille --corriger` dans les réglages du bureau / compositeur.
    !selection::is_wayland()
}

// ---------------------------------------------------------------------------
// Commandes appelées depuis l'interface
// ---------------------------------------------------------------------------

#[tauri::command]
async fn check_text(state: State<'_, AppState>, text: String, language: String) -> Result<langtool::CheckResult, String> {
    let cfg = state.config.lock().unwrap().clone();
    langtool::check(&state.http, &cfg, &text, &language).await
}

#[tauri::command]
async fn list_languages(state: State<'_, AppState>) -> Result<Vec<langtool::LtLang>, String> {
    let url = state.config.lock().unwrap().server_url.clone();
    langtool::languages(&state.http, &url).await
}

#[tauri::command]
async fn test_server(state: State<'_, AppState>, url: String) -> Result<String, String> {
    let url = config::normalize_server_url(&url);
    let langs = langtool::languages(&state.http, &url).await?;
    let french = langs.iter().any(|l| l.code == "fr");
    Ok(format!(
        "Connexion réussie à {url} : {} langues disponibles{}.",
        langs.len(),
        if french { ", dont le français" } else { " (attention : pas de français)" }
    ))
}

#[tauri::command]
fn get_config(state: State<AppState>) -> Config {
    state.config.lock().unwrap().clone()
}

fn persist(state: &AppState, cfg: Config) -> Result<Config, String> {
    cfg.save(&state.config_path)?;
    *state.config.lock().unwrap() = cfg.clone();
    Ok(cfg)
}

#[tauri::command]
fn save_config(app: AppHandle, state: State<AppState>, config: Config) -> Result<Config, String> {
    let new = config.normalized();
    let old = state.config.lock().unwrap().clone();
    let had_error = state.shortcut_error.lock().unwrap().is_some();
    if shortcut_supported() && (new.shortcut != old.shortcut || had_error) {
        if let Err(e) = register_shortcut(&app, &new.shortcut) {
            // On remet l'ancien raccourci pour ne pas laisser l'utilisateur sans rien.
            let _ = register_shortcut(&app, &old.shortcut);
            return Err(e);
        }
        *state.shortcut_error.lock().unwrap() = None;
    }
    persist(&state, new)
}

/// Désactive le raccourci global le temps d'en enregistrer un nouveau dans les paramètres.
#[tauri::command]
fn suspend_shortcut(app: AppHandle) {
    if shortcut_supported() {
        let _ = app.global_shortcut().unregister_all();
    }
}

#[tauri::command]
fn resume_shortcut(app: AppHandle) -> Option<String> {
    if !shortcut_supported() {
        return None;
    }
    let state = app.state::<AppState>();
    let wanted = state.config.lock().unwrap().shortcut.clone();
    let err = register_shortcut(&app, &wanted).err();
    *state.shortcut_error.lock().unwrap() = err.clone();
    err
}

#[tauri::command]
fn add_to_dictionary(state: State<AppState>, word: String) -> Result<Config, String> {
    let mut cfg = state.config.lock().unwrap().clone();
    cfg.dictionary.push(word);
    persist(&state, cfg.normalized())
}

#[tauri::command]
fn disable_rule(state: State<AppState>, rule_id: String) -> Result<Config, String> {
    let mut cfg = state.config.lock().unwrap().clone();
    cfg.disabled_rules.push(rule_id);
    persist(&state, cfg.normalized())
}

#[tauri::command]
fn take_pending(state: State<AppState>) -> Option<Capture> {
    state.pending.lock().unwrap().take()
}

#[tauri::command]
fn copy_text(text: String) -> Result<(), String> {
    selection::set_clipboard(&text)
}

#[tauri::command]
fn hide_window(app: AppHandle) {
    hide_main(&app);
}

#[tauri::command]
async fn apply_text(app: AppHandle, state: State<'_, AppState>, text: String) -> Result<ApplyResult, String> {
    let target = *state.target.lock().unwrap();
    let restore = state.config.lock().unwrap().restore_clipboard;
    let select_all = state.replace_field.load(Ordering::SeqCst);
    hide_main(&app);
    #[cfg(target_os = "macos")]
    let _ = app.hide(); // rend la main à l'application précédente
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        target.restore();
        std::thread::sleep(Duration::from_millis(250));
        selection::paste(&text, restore, select_all)
    })
    .await
    .map_err(|e| e.to_string())?;
    match outcome {
        Ok(selection::PasteOutcome::Pasted) => Ok(ApplyResult { pasted: true, message: None }),
        Ok(selection::PasteOutcome::CopiedOnly(msg)) => {
            show_main(&app);
            Ok(ApplyResult { pasted: false, message: Some(msg) })
        }
        Err(e) => {
            show_main(&app);
            Err(e)
        }
    }
}

fn send_bubble(state: &AppState, cmd: bubble::Command) {
    if let Some(tx) = state.bubble_tx.lock().unwrap().as_ref() {
        let _ = tx.send(cmd);
    }
}

#[tauri::command]
fn bubble_get_state(state: State<AppState>) -> Option<bubble::BubbleState> {
    state.bubble_state.lock().unwrap().clone()
}

#[tauri::command]
fn bubble_expand(state: State<AppState>, expanded: bool) {
    send_bubble(&state, bubble::Command::Expand(expanded));
}

#[tauri::command]
fn bubble_apply(state: State<AppState>, key: u64, replacement: String) {
    send_bubble(&state, bubble::Command::Apply { key, replacement });
}

#[tauri::command]
fn bubble_drop_rule(state: State<AppState>, rule_id: String) {
    send_bubble(&state, bubble::Command::DropRule(rule_id));
}

#[tauri::command]
fn bubble_drop_word(state: State<AppState>, word: String) {
    send_bubble(&state, bubble::Command::DropWord(word));
}

#[tauri::command]
fn bubble_ignore(state: State<AppState>, rule_id: String, original: String) {
    send_bubble(&state, bubble::Command::Ignore { rule_id, original });
}

#[tauri::command]
fn bubble_recheck(state: State<AppState>) {
    send_bubble(&state, bubble::Command::Recheck);
}

#[tauri::command]
fn bubble_open_editor(state: State<AppState>) {
    send_bubble(&state, bubble::Command::OpenEditor);
}

#[tauri::command]
fn platform_info(state: State<AppState>) -> PlatformInfo {
    let shortcut_error = state.shortcut_error.lock().unwrap().clone();
    PlatformInfo {
        os: std::env::consts::OS,
        wayland: selection::is_wayland(),
        shortcut_active: shortcut_supported() && shortcut_error.is_none(),
        shortcut_error,
        shortcut_notice: state.shortcut_notice.lock().unwrap().take(),
        bubble_supported: cfg!(windows),
        config_path: state.config_path.display().to_string(),
        version: env!("CARGO_PKG_VERSION"),
    }
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let al = app.autolaunch();
    if enabled { al.enable() } else { al.disable() }.map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Icône de la barre système
// ---------------------------------------------------------------------------

fn build_tray(app: &AppHandle, shortcut_label: &str) -> tauri::Result<()> {
    let check_label = if shortcut_supported() {
        format!("Corriger la sélection ({shortcut_label})")
    } else {
        "Corriger la sélection".to_string()
    };
    let check = MenuItem::with_id(app, "check", check_label, true, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Ouvrir l'éditeur", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Paramètres…", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quitter Coquille", true, None::<&str>)?;
    // Sous Windows, cliquer dans le menu de la zone de notification fait perdre la sélection
    // de l'application d'origine : l'entrée « Corriger la sélection » n'y aurait pas de sens.
    let menu = if cfg!(windows) {
        Menu::with_items(app, &[&open, &settings, &sep, &quit])?
    } else {
        Menu::with_items(app, &[&check, &open, &settings, &sep, &quit])?
    };

    let mut builder = TrayIconBuilder::with_id("coquille")
        .tooltip("Coquille — correcteur")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "check" => trigger_capture(app),
            "open" => show_main(app),
            "settings" => {
                show_main(app);
                let _ = app.emit_to("main", "open-settings", ());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

// ---------------------------------------------------------------------------

fn main() {
    // WebKitGTK : évite la fenêtre blanche connue avec certains pilotes (NVIDIA sous Wayland notamment).
    // Coût négligeable pour une interface aussi légère ; respecte la valeur si l'utilisateur l'a déjà définie.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    tauri::Builder::default()
        // Doit être le premier plugin : une 2e instance transmet ses arguments à la 1re.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            handle_args(app, &argv, false);
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    // Déclenché au relâchement pour ne pas « taper » la touche dans l'application cible.
                    if event.state() == ShortcutState::Released {
                        trigger_capture(app);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--cache"])))
        .setup(|app| {
            let config_path = app.path().app_config_dir()?.join("config.json");
            let cfg = Config::load(&config_path);
            app.manage(AppState {
                config: Mutex::new(cfg),
                config_path,
                http: langtool::http_client(),
                pending: Mutex::new(None),
                target: Mutex::new(selection::Target::default()),
                capturing: AtomicBool::new(false),
                capture_seq: AtomicU64::new(0),
                shortcut_error: Mutex::new(None),
                shortcut_notice: Mutex::new(None),
                bubble_tx: Mutex::new(None),
                bubble_state: Mutex::new(None),
                replace_field: AtomicBool::new(false),
            });

            let handle = app.handle().clone();
            if shortcut_supported() {
                register_configured_shortcut(&handle);
            }
            let shortcut = app.state::<AppState>().config.lock().unwrap().shortcut.clone();
            if let Err(e) = build_tray(&handle, &shortcut) {
                // Pas de zone de notification (ex. GNOME sans extension AppIndicator) : l'app reste utilisable.
                eprintln!("Icône de notification indisponible : {e}");
            }
            #[cfg(windows)]
            {
                // Bulle : petite fenêtre transparente, toujours au premier plan, qui ne prend jamais le focus.
                let bubble_window = tauri::WebviewWindowBuilder::new(app, "bubble", tauri::WebviewUrl::App("bubble.html".into()))
                    .title("Coquille — bulle")
                    .inner_size(30.0, 30.0)
                    .decorations(false)
                    .transparent(true)
                    .shadow(false)
                    .always_on_top(true)
                    .skip_taskbar(true)
                    .resizable(false)
                    .focusable(false)
                    .focused(false)
                    .visible(false)
                    .build();
                match bubble_window {
                    Ok(_) => {
                        let tx = bubble::spawn(handle.clone());
                        *app.state::<AppState>().bubble_tx.lock().unwrap() = Some(tx);
                    }
                    Err(e) => eprintln!("Bulle indisponible : {e}"),
                }
            }
            let argv: Vec<String> = std::env::args().collect();
            handle_args(&handle, &argv, true);
            Ok(())
        })
        .on_window_event(|window, event| {
            // Fermer la fenêtre la cache seulement : Coquille reste disponible via le raccourci.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            check_text,
            list_languages,
            test_server,
            get_config,
            save_config,
            add_to_dictionary,
            disable_rule,
            suspend_shortcut,
            resume_shortcut,
            bubble_get_state,
            bubble_expand,
            bubble_apply,
            bubble_drop_rule,
            bubble_drop_word,
            bubble_ignore,
            bubble_recheck,
            bubble_open_editor,
            take_pending,
            copy_text,
            hide_window,
            apply_text,
            platform_info,
            get_autostart,
            set_autostart,
        ])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de Coquille");
}
