//! Capture du texte sélectionné dans l'application active et collage du texte corrigé.
//!
//! - Linux : lecture de la sélection PRIMAIRE (le texte surligné), sans toucher au
//!   presse-papiers. Wayland via `wl-paste`/`wl-copy` (+ `wtype` ou `ydotool` pour
//!   coller), X11 via arboard + enigo.
//! - Windows / macOS : simulation de Ctrl+C / Cmd+C, puis restauration du presse-papiers.

#![allow(dead_code)]

use std::thread::sleep;
use std::time::{Duration, Instant};

pub const NO_SELECTION: &str =
    "Aucun texte sélectionné n'a été détecté. Sélectionne du texte puis réutilise le raccourci, ou écris directement ici.";

pub enum PasteOutcome {
    /// Le texte a été collé dans l'application d'origine.
    Pasted,
    /// Le texte est dans le presse-papiers mais n'a pas pu être collé automatiquement.
    CopiedOnly(String),
}

// ---------------------------------------------------------------------------
// Fenêtre cible (pour lui rendre le focus avant de coller)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy)]
pub struct Target {
    #[cfg(windows)]
    hwnd: isize,
}

impl Target {
    pub fn current() -> Self {
        #[cfg(windows)]
        {
            let hwnd = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
            Target { hwnd: hwnd as isize }
        }
        #[cfg(not(windows))]
        {
            Target::default()
        }
    }

    #[cfg(windows)]
    pub fn from_hwnd(hwnd: isize) -> Self {
        Target { hwnd }
    }

    pub fn restore(&self) {
        #[cfg(windows)]
        if self.hwnd != 0 {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::SetForegroundWindow(self.hwnd as _);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// API publique
// ---------------------------------------------------------------------------

/// Récupère le texte actuellement sélectionné. `Ok(None)` = rien de sélectionné.
pub fn capture() -> Result<Option<String>, String> {
    platform::capture()
}

/// Colle `text` à la place de la sélection dans l'application qui a le focus.
/// Avec `select_all`, sélectionne d'abord tout le contenu du champ (Ctrl+A).
pub fn paste(text: &str, restore_clipboard: bool, select_all: bool) -> Result<PasteOutcome, String> {
    let previous = platform::get_clipboard_text();
    platform::set_clipboard_text(text)?;
    sleep(Duration::from_millis(120));
    if select_all {
        if let Err(msg) = platform::send_select_all() {
            return Ok(PasteOutcome::CopiedOnly(msg));
        }
        sleep(Duration::from_millis(80));
    }
    match platform::send_paste() {
        Ok(()) => {
            if restore_clipboard {
                if let Some(prev) = previous {
                    let ours = text.to_string();
                    std::thread::spawn(move || {
                        // Laisse le temps à l'application cible de lire le presse-papiers.
                        sleep(Duration::from_millis(1500));
                        // Ne restaure que si personne n'a copié autre chose entre-temps.
                        if platform::get_clipboard_text().as_deref() == Some(ours.as_str()) {
                            let _ = platform::set_clipboard_text(&prev);
                        }
                    });
                }
            }
            Ok(PasteOutcome::Pasted)
        }
        Err(msg) => Ok(PasteOutcome::CopiedOnly(msg)),
    }
}

pub fn set_clipboard(text: &str) -> Result<(), String> {
    platform::set_clipboard_text(text)
}

// ---------------------------------------------------------------------------
// Utilitaires communs
// ---------------------------------------------------------------------------

#[allow(unused)]
fn send_shortcut(letter: char) -> Result<(), String> {
    use enigo::{Direction, Enigo, Key, Keyboard, NewConError, Settings};

    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| match e {
        NewConError::NoPermission => "Coquille n'a pas l'autorisation de contrôler le clavier. \
            Ouvre Réglages Système › Confidentialité et sécurité › Accessibilité, active Coquille, puis relance l'application."
            .to_string(),
        other => format!("Simulation du clavier impossible : {other:?}"),
    })?;

    #[cfg(target_os = "macos")]
    let (modifier, key) = (Key::Meta, Key::Unicode(letter));
    // Code de touche virtuelle (VK_C = 0x43, VK_V = 0x56) : indépendant de la disposition.
    #[cfg(windows)]
    let (modifier, key) = (Key::Control, Key::Other(letter.to_ascii_uppercase() as u32));
    #[cfg(not(any(target_os = "macos", windows)))]
    let (modifier, key) = (Key::Control, Key::Unicode(letter));

    let err = |e: enigo::InputError| format!("Simulation du clavier impossible : {e:?}");
    enigo.key(modifier, Direction::Press).map_err(err)?;
    sleep(Duration::from_millis(15));
    let res = enigo.key(key, Direction::Click).map_err(err);
    sleep(Duration::from_millis(15));
    enigo.key(modifier, Direction::Release).map_err(err)?;
    res
}

/// Attend que l'utilisateur relâche les touches de modification du raccourci,
/// sinon Ctrl+C simulé deviendrait Ctrl+Alt+C (et redéclencherait le raccourci).
#[allow(unused)]
fn wait_modifiers_released(timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while platform::modifiers_down() && Instant::now() < deadline {
        sleep(Duration::from_millis(15));
    }
    sleep(Duration::from_millis(40));
}

// ---------------------------------------------------------------------------
// Windows & macOS : copie simulée
// ---------------------------------------------------------------------------

#[cfg(any(windows, target_os = "macos"))]
mod platform {
    use super::*;
    use arboard::Clipboard;

    fn clipboard() -> Result<Clipboard, String> {
        Clipboard::new().map_err(|e| format!("Presse-papiers inaccessible : {e}"))
    }

    pub fn get_clipboard_text() -> Option<String> {
        clipboard().ok()?.get_text().ok()
    }

    pub fn set_clipboard_text(text: &str) -> Result<(), String> {
        clipboard()?
            .set_text(text.to_string())
            .map_err(|e| format!("Impossible d'écrire dans le presse-papiers : {e}"))
    }

    #[cfg(windows)]
    fn change_count() -> i64 {
        unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() as i64 }
    }

    #[cfg(target_os = "macos")]
    #[allow(unused_unsafe)]
    fn change_count() -> i64 {
        unsafe { objc2_app_kit::NSPasteboard::generalPasteboard().changeCount() as i64 }
    }

    #[cfg(windows)]
    pub fn modifiers_down() -> bool {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
        };
        [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
            .iter()
            .any(|&vk| ((unsafe { GetAsyncKeyState(vk as i32) } as u16) & 0x8000) != 0)
    }

    #[cfg(target_os = "macos")]
    pub fn modifiers_down() -> bool {
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGEventSourceFlagsState(state_id: i32) -> u64;
        }
        // kCGEventSourceStateHIDSystemState = 1 ; masques Shift, Control, Option, Command.
        const MASK: u64 = 0x0002_0000 | 0x0004_0000 | 0x0008_0000 | 0x0010_0000;
        (unsafe { CGEventSourceFlagsState(1) } & MASK) != 0
    }

    pub fn capture() -> Result<Option<String>, String> {
        wait_modifiers_released(Duration::from_millis(1500));
        let cb_before = change_count();
        let backup = get_clipboard_text();

        send_shortcut('c')?;

        // L'application cible met parfois un peu de temps à remplir le presse-papiers.
        let deadline = Instant::now() + Duration::from_millis(800);
        let mut captured = None;
        while Instant::now() < deadline {
            sleep(Duration::from_millis(30));
            if change_count() != cb_before {
                sleep(Duration::from_millis(30));
                captured = get_clipboard_text();
                break;
            }
        }

        // On remet le presse-papiers tel qu'il était (contenu texte uniquement).
        if captured.is_some() {
            if let Some(prev) = backup {
                let _ = set_clipboard_text(&prev);
            }
        }
        Ok(captured.filter(|t| !t.is_empty()))
    }

    pub fn send_paste() -> Result<(), String> {
        send_shortcut('v')
    }

    pub fn send_select_all() -> Result<(), String> {
        send_shortcut('a')
    }
}

// ---------------------------------------------------------------------------
// Linux (et autres Unix) : sélection primaire
// ---------------------------------------------------------------------------

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use super::*;
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::sync::Mutex;

    pub fn is_wayland() -> bool {
        std::env::var_os("WAYLAND_DISPLAY").is_some()
    }

    pub fn modifiers_down() -> bool {
        false
    }

    // Sous X11, le contenu du presse-papiers est servi par le processus qui l'a défini :
    // on garde donc une instance arboard vivante pendant toute la durée de l'application.
    static X11_CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

    fn with_x11<T>(f: impl FnOnce(&mut arboard::Clipboard) -> Result<T, String>) -> Result<T, String> {
        let mut guard = X11_CLIPBOARD.lock().map_err(|_| "Presse-papiers verrouillé".to_string())?;
        if guard.is_none() {
            *guard = Some(arboard::Clipboard::new().map_err(|e| format!("Presse-papiers X11 inaccessible : {e}"))?);
        }
        f(guard.as_mut().expect("initialisé"))
    }

    pub enum RunError {
        NotFound,
        Failed(String),
    }

    /// Lance une commande avec délai maximal. Renvoie (succès, stdout).
    fn run(cmd: &str, args: &[&str], input: Option<&str>, capture_output: bool, timeout: Duration) -> Result<(bool, String), RunError> {
        let mut command = Command::new(cmd);
        command
            .args(args)
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            // wl-copy se détache en arrière-plan : ne pas capturer sa sortie, sinon le tube reste ouvert.
            .stdout(if capture_output { Stdio::piped() } else { Stdio::null() })
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                RunError::NotFound
            } else {
                RunError::Failed(format!("{cmd} : {e}"))
            }
        })?;
        if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
            let _ = stdin.write_all(text.as_bytes());
        }
        let reader = child.stdout.take().map(|mut out| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = out.read_to_end(&mut buf);
                String::from_utf8_lossy(&buf).into_owned()
            })
        });
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => sleep(Duration::from_millis(10)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RunError::Failed(format!("{cmd} ne répond pas")));
                }
                Err(e) => return Err(RunError::Failed(format!("{cmd} : {e}"))),
            }
        };
        let out = reader.and_then(|h| h.join().ok()).unwrap_or_default();
        Ok((status.success(), out))
    }

    pub fn get_clipboard_text() -> Option<String> {
        if is_wayland() {
            match run("wl-paste", &["--no-newline", "--type", "text"], None, true, Duration::from_secs(2)) {
                Ok((true, s)) => Some(s),
                _ => None,
            }
        } else {
            with_x11(|cb| cb.get_text().map_err(|e| e.to_string())).ok()
        }
    }

    pub fn set_clipboard_text(text: &str) -> Result<(), String> {
        if is_wayland() {
            match run("wl-copy", &[], Some(text), false, Duration::from_secs(3)) {
                Ok((true, _)) => Ok(()),
                Ok((false, _)) => Err("wl-copy a échoué".into()),
                Err(RunError::NotFound) => Err(MISSING_WL_CLIPBOARD.into()),
                Err(RunError::Failed(e)) => Err(e),
            }
        } else {
            with_x11(|cb| cb.set_text(text.to_string()).map_err(|e| format!("Presse-papiers X11 : {e}")))
        }
    }

    const MISSING_WL_CLIPBOARD: &str =
        "wl-clipboard est requis sous Wayland : sudo pacman -S wl-clipboard (ou apt/dnf install wl-clipboard).";

    pub fn capture() -> Result<Option<String>, String> {
        if is_wayland() {
            match run("wl-paste", &["--primary", "--no-newline", "--type", "text"], None, true, Duration::from_secs(2)) {
                Ok((true, s)) => Ok(Some(s).filter(|s| !s.is_empty())),
                Ok((false, _)) => Ok(None),
                Err(RunError::NotFound) => Err(MISSING_WL_CLIPBOARD.into()),
                Err(RunError::Failed(e)) => Err(e),
            }
        } else {
            use arboard::{GetExtLinux, LinuxClipboardKind};
            match with_x11(|cb| {
                cb.get()
                    .clipboard(LinuxClipboardKind::Primary)
                    .text()
                    .map_err(|e| e.to_string())
            }) {
                Ok(s) => Ok(Some(s).filter(|s| !s.is_empty())),
                Err(_) => Ok(None),
            }
        }
    }

    pub fn send_paste() -> Result<(), String> {
        if !is_wayland() {
            return send_shortcut('v');
        }
        // wtype : compositeurs wlroots (Hyprland, Sway, river…).
        // « -s 120 » : petite pause avant la 1re touche, sinon certains compositeurs perdent
        // le premier événement (le Ctrl) le temps d'appliquer la disposition du clavier virtuel.
        if let Ok((true, _)) =
            run("wtype", &["-s", "120", "-M", "ctrl", "-k", "v", "-m", "ctrl"], None, false, Duration::from_secs(3))
        {
            return Ok(());
        }
        // ydotool : fonctionne partout (KDE, GNOME…) via uinput, nécessite le service ydotoold.
        // 29 = KEY_LEFTCTRL, 47 = KEY_V.
        if let Ok((true, _)) = run("ydotool", &["key", "29:1", "47:1", "47:0", "29:0"], None, false, Duration::from_secs(2)) {
            return Ok(());
        }
        Err("Texte corrigé copié dans le presse-papiers : colle-le avec Ctrl+V. \
             Pour le collage automatique sous Wayland, installe wtype (Hyprland, Sway) ou ydotool (KDE, GNOME)."
            .into())
    }

    pub fn send_select_all() -> Result<(), String> {
        if !is_wayland() {
            return send_shortcut('a');
        }
        if let Ok((true, _)) =
            run("wtype", &["-s", "120", "-M", "ctrl", "-k", "a", "-m", "ctrl"], None, false, Duration::from_secs(3))
        {
            return Ok(());
        }
        // 30 = KEY_A
        if let Ok((true, _)) = run("ydotool", &["key", "29:1", "30:1", "30:0", "29:0"], None, false, Duration::from_secs(2)) {
            return Ok(());
        }
        Err("Sélection automatique impossible : texte corrigé copié, sélectionne le champ puis colle avec Ctrl+V.".into())
    }
}

pub fn is_wayland() -> bool {
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        platform::is_wayland()
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        false
    }
}
