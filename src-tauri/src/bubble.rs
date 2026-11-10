//! Bulle de correction en direct (Windows).
//!
//! Un thread surveille le champ de saisie qui a le focus dans n'importe quelle application
//! grâce à UI Automation (l'API d'accessibilité de Windows), envoie son texte à LanguageTool
//! quand la frappe se calme, et affiche une petite pastille non activable (elle ne vole pas
//! le focus) au bord du champ, à la hauteur du curseur. Un clic ouvre la liste des fautes ;
//! cliquer une suggestion sélectionne le mot fautif dans le champ et tape la correction.

#![cfg_attr(not(windows), allow(dead_code))]

use crate::langtool::Issue;
use serde::Serialize;

/// État envoyé à la fenêtre « bubble ».
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct BubbleState {
    /// "checking" | "ok" | "issues" | "error"
    pub status: String,
    pub issues: Vec<Issue>,
    pub error: Option<String>,
    pub message: Option<String>,
    pub expanded: bool,
    /// Le texte a changé depuis la dernière vérification.
    pub stale: bool,
    pub app: String,
}

/// Ordres envoyés au thread de surveillance par l'interface de la bulle.
#[derive(Debug)]
pub enum Command {
    /// Applique une suggestion à la faute identifiée par sa clé.
    Apply { key: u64, replacement: String },
    Expand(bool),
    Ignore { rule_id: String, original: String },
    /// Retire localement les fautes d'une règle désactivée (sans revérifier).
    DropRule(String),
    /// Retire localement les fautes d'orthographe d'un mot ajouté au dictionnaire.
    DropWord(String),
    Recheck,
    OpenEditor,
}

/// Met à jour la liste des fautes après le remplacement de `[offset, offset+length)` par
/// `repl_len` unités : les fautes suivantes sont décalées, celles qui chevauchent la zone
/// remplacée disparaissent. L'analyse de la phrase d'origine est conservée telle quelle :
/// pas de nouvelle vérification, donc pas de liste qui se réorganise à chaque clic.
pub fn rebase_issues(issues: Vec<Issue>, offset: usize, length: usize, repl_len: usize) -> Vec<Issue> {
    let end = offset + length;
    issues
        .into_iter()
        .filter(|i| i.offset + i.length <= offset || i.offset >= end)
        .map(|mut i| {
            if i.offset >= end {
                i.offset = i.offset + repl_len - length;
            }
            i
        })
        .collect()
}

/// Nombre de mots minimal pour vérifier un champ : évite d'analyser barres d'adresse et recherches.
pub const MIN_WORDS: usize = 3;

pub fn word_count(text: &str) -> usize {
    text.split_whitespace().filter(|w| w.chars().any(char::is_alphanumeric)).count()
}

/// Unités de comptage possibles des « caractères » UI Automation selon les applications.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Units {
    Utf16,
    Scalar,
    CrLfAsOne,
}

pub fn count_units(s: &str, units: Units) -> usize {
    match units {
        Units::Utf16 => s.encode_utf16().count(),
        Units::Scalar => s.chars().count(),
        Units::CrLfAsOne => s.encode_utf16().count() - s.matches("\r\n").count(),
    }
}

/// Remplace `[offset, offset+length)` (unités UTF-16) par `replacement`.
pub fn splice_utf16(text: &str, offset: usize, length: usize, replacement: &str) -> Option<String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let end = offset.checked_add(length)?;
    if end > units.len() {
        return None;
    }
    let mut out = String::from_utf16_lossy(&units[..offset]);
    out.push_str(replacement);
    out.push_str(&String::from_utf16_lossy(&units[end..]));
    Some(out)
}

/// Nouvelle position du curseur (UTF-16) après remplacement de `[offset, offset+length)`.
pub fn shift_caret(caret: usize, offset: usize, length: usize, repl_len: usize) -> usize {
    if caret >= offset + length {
        caret + repl_len - length
    } else if caret <= offset {
        caret
    } else {
        offset + repl_len
    }
}

#[cfg(windows)]
pub use imp::spawn;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::{config::Config, langtool, AppState};
    use std::collections::{HashMap, HashSet};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::thread::sleep;
    use std::time::{Duration, Instant};
    use tauri::{AppHandle, Emitter, Manager};
    use windows::core::BSTR;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTextPattern, IUIAutomationTextRange,
        IUIAutomationValuePattern, TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start, TextUnit_Character,
        UIA_DocumentControlTypeId, UIA_EditControlTypeId, UIA_TextPatternId, UIA_ValuePatternId,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, POINT};
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, SetForegroundWindow, SetWindowPos, ShowWindow,
        GUITHREADINFO, HWND_TOPMOST, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE,
    };

    const POLL: Duration = Duration::from_millis(350);
    const DEBOUNCE: Duration = Duration::from_millis(900);
    const RETRY_AFTER_ERROR: Duration = Duration::from_secs(10);
    /// Limite du serveur public (≈ 20 Ko par requête) ; au-delà, on laisse le raccourci prendre le relais.
    const MAX_UTF16: usize = 15_000;
    const BUBBLE_SIZE: f64 = 30.0;
    const PANEL_W: f64 = 360.0;
    const PANEL_H: f64 = 430.0;
    /// Applications dont le contrôle « Document » est un vrai éditeur (et pas une page web en lecture).
    const DOC_EDITORS: &[&str] = &["winword.exe", "notepad.exe", "wordpad.exe", "soffice.bin", "onenote.exe", "outlook.exe", "olk.exe"];
    /// Barres d'adresse des navigateurs : jamais de bulle.
    const EXCLUDED_CLASSES: &[&str] = &["OmniboxViewViews"];
    const EXCLUDED_IDS: &[&str] = &["urlbar-input"];

    /// Instant situé `d` dans le passé (sans risque de dépassement juste après le démarrage).
    fn ago(d: Duration) -> Instant {
        let now = Instant::now();
        now.checked_sub(d).unwrap_or(now)
    }

    #[derive(Clone, Copy, PartialEq, Debug)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    struct Field {
        element: IUIAutomationElement,
        hwnd: isize,
        app: String,
        text: String,
        rect: Rect,
        checked_text: Option<String>,
        changed_at: Instant,
        issues: Vec<Issue>,
        error: Option<String>,
    }

    struct Watcher {
        app: AppHandle,
        uia: IUIAutomation,
        field: Option<Field>,
        expanded: bool,
        ignored: HashSet<(String, String)>,
        names: HashMap<u32, String>,
        last_request: Option<Instant>,
        visible: bool,
        last_pos: Option<(i32, i32)>,
        message: Option<(String, Instant)>,
        last_emitted: Option<BubbleState>,
        next_key: u64,
    }

    pub fn spawn(app: AppHandle) -> Sender<Command> {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("coquille-bulle".into())
            .spawn(move || {
                if let Err(e) = run(app, rx) {
                    eprintln!("Bulle de correction désactivée : {e}");
                }
            })
            .expect("thread de la bulle");
        tx
    }

    fn run(app: AppHandle, rx: Receiver<Command>) -> windows::core::Result<()> {
        // Client UI Automation : thread dédié, appartement multithread (recommandation Microsoft).
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        let uia: IUIAutomation = unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
        let mut w = Watcher {
            app,
            uia,
            field: None,
            expanded: false,
            ignored: HashSet::new(),
            names: HashMap::new(),
            last_request: None,
            visible: false,
            last_pos: None,
            message: None,
            last_emitted: None,
            next_key: 0,
        };
        loop {
            match rx.recv_timeout(POLL) {
                Ok(cmd) => w.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            w.tick();
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Lecture du champ actif
    // ------------------------------------------------------------------

    fn window_pid(hwnd: isize) -> u32 {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd as _, &mut pid) };
        pid
    }

    fn process_name(pid: u32) -> String {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return String::new();
            }
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
            CloseHandle(handle);
            if ok == 0 {
                return String::new();
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            path.rsplit(['\\', '/']).next().unwrap_or_default().to_string()
        }
    }

    unsafe fn value_pattern(el: &IUIAutomationElement) -> Option<IUIAutomationValuePattern> {
        unsafe { el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId).ok() }
    }

    unsafe fn text_pattern(el: &IUIAutomationElement) -> Option<IUIAutomationTextPattern> {
        unsafe { el.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId).ok() }
    }

    unsafe fn read_text(el: &IUIAutomationElement) -> Option<String> {
        unsafe {
            if let Some(tp) = text_pattern(el) {
                if let Ok(range) = tp.DocumentRange() {
                    if let Ok(text) = range.GetText((MAX_UTF16 + 1) as i32) {
                        return Some(text.to_string());
                    }
                }
            }
            value_pattern(el)?.CurrentValue().ok().map(|v| v.to_string())
        }
    }

    /// Renvoie le texte et le rectangle du champ s'il s'agit d'une zone de saisie modifiable.
    unsafe fn read_editable(el: &IUIAutomationElement, app: &str) -> Option<(String, Rect)> {
        unsafe {
            // Jamais de mots de passe.
            if el.CurrentIsPassword().map(|b| b.as_bool()).unwrap_or(true) {
                return None;
            }
            let class = el.CurrentClassName().map(|s| s.to_string()).unwrap_or_default();
            let auto_id = el.CurrentAutomationId().map(|s| s.to_string()).unwrap_or_default();
            if EXCLUDED_CLASSES.contains(&class.as_str()) || EXCLUDED_IDS.contains(&auto_id.as_str()) {
                return None;
            }
            let control = el.CurrentControlType().ok()?;
            let read_only = value_pattern(el).and_then(|v| v.CurrentIsReadOnly().ok()).map(|b| b.as_bool());
            let editable = if control == UIA_EditControlTypeId {
                read_only != Some(true)
            } else if control == UIA_DocumentControlTypeId {
                DOC_EDITORS.iter().any(|d| d.eq_ignore_ascii_case(app)) && read_only != Some(true)
            } else {
                false
            };
            if !editable {
                return None;
            }
            let text = read_text(el)?;
            let r = el.CurrentBoundingRectangle().ok()?;
            if r.right <= r.left || r.bottom <= r.top {
                return None;
            }
            Some((text, Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom }))
        }
    }

    /// Position du curseur texte (caret système) en coordonnées écran, si l'application l'expose.
    fn caret_rect(hwnd: isize) -> Option<Rect> {
        unsafe {
            let thread = GetWindowThreadProcessId(hwnd as _, std::ptr::null_mut());
            let mut info: GUITHREADINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
            if GetGUIThreadInfo(thread, &mut info) == 0 || info.hwndCaret.is_null() {
                return None;
            }
            let r = info.rcCaret;
            if r.bottom <= r.top {
                return None;
            }
            let mut a = POINT { x: r.left, y: r.top };
            let mut b = POINT { x: r.right, y: r.bottom };
            ClientToScreen(info.hwndCaret, &mut a);
            ClientToScreen(info.hwndCaret, &mut b);
            Some(Rect { left: a.x, top: a.y, right: b.x, bottom: b.y })
        }
    }

    /// Plage de texte `[start, start+len)` exprimée en « caractères » UI Automation.
    unsafe fn range_at(doc: &IUIAutomationTextRange, start: usize, len: usize) -> Option<IUIAutomationTextRange> {
        unsafe {
            let r = doc.Clone().ok()?;
            // Plage vide au début du document, puis on avance la fin, puis le début.
            r.MoveEndpointByRange(TextPatternRangeEndpoint_End, &r, TextPatternRangeEndpoint_Start).ok()?;
            if start + len > 0 {
                r.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, (start + len) as i32).ok()?;
            }
            if start > 0 {
                r.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, start as i32).ok()?;
            }
            Some(r)
        }
    }

    /// Texte situé avant le curseur (pour remettre le curseur à sa place après une correction).
    unsafe fn text_before_caret(tp: &IUIAutomationTextPattern, doc: &IUIAutomationTextRange) -> Option<String> {
        unsafe {
            let selection = tp.GetSelection().ok()?;
            if selection.Length().ok()? < 1 {
                return None;
            }
            let sel = selection.GetElement(0).ok()?;
            let before = doc.Clone().ok()?;
            before.MoveEndpointByRange(TextPatternRangeEndpoint_End, &sel, TextPatternRangeEndpoint_Start).ok()?;
            before.GetText(-1).ok().map(|t| t.to_string())
        }
    }

    /// Place (et affiche) la fenêtre de la bulle sans jamais l'activer : le focus reste dans
    /// l'application où l'utilisateur écrit. Tauri utiliserait SW_SHOW, qui peut voler le focus.
    fn place_window(win: &tauri::WebviewWindow, x: i32, y: i32, w: i32, h: i32) {
        if let Ok(hwnd) = win.hwnd() {
            unsafe {
                SetWindowPos(hwnd.0 as _, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            }
        }
    }

    fn hide_window(win: &tauri::WebviewWindow) {
        if let Ok(hwnd) = win.hwnd() {
            unsafe {
                ShowWindow(hwnd.0 as _, SW_HIDE);
            }
        }
    }

    fn type_text(text: &str) -> Result<(), String> {
        use enigo::{Direction, Enigo, Key, Keyboard, Settings};
        let mut enigo = Enigo::new(&Settings::default()).map_err(|e| format!("Clavier virtuel indisponible : {e:?}"))?;
        if text.is_empty() {
            enigo.key(Key::Delete, Direction::Click).map_err(|e| format!("{e:?}"))
        } else {
            enigo.text(text).map_err(|e| format!("{e:?}"))
        }
    }

    // ------------------------------------------------------------------

    impl Watcher {
        fn config(&self) -> Config {
            self.app.state::<AppState>().config.lock().unwrap().clone()
        }

        fn name_of(&mut self, pid: u32) -> String {
            self.names.entry(pid).or_insert_with(|| process_name(pid)).clone()
        }

        fn window(&self) -> Option<tauri::WebviewWindow> {
            self.app.get_webview_window("bubble")
        }

        fn hide(&mut self) {
            if self.visible {
                if let Some(w) = self.window() {
                    hide_window(&w);
                }
                self.visible = false;
                self.last_pos = None;
            }
            if self.expanded {
                self.set_expanded(false);
            }
        }

        fn lose_field(&mut self) {
            self.field = None;
            self.hide();
        }

        fn tick(&mut self) {
            let cfg = self.config();
            if !cfg.bubble {
                self.lose_field();
                return;
            }
            let fg = unsafe { GetForegroundWindow() } as isize;
            let fg_pid = window_pid(fg);
            if fg_pid == std::process::id() {
                // L'éditeur de Coquille (ou le panneau de la bulle) est au premier plan.
                if !self.expanded {
                    self.hide();
                }
                return;
            }
            let Ok(el) = (unsafe { self.uia.GetFocusedElement() }) else {
                self.lose_field();
                return;
            };
            let app_name = self.name_of(fg_pid);
            if cfg.bubble_excluded_apps.iter().any(|x| x.eq_ignore_ascii_case(&app_name)) {
                self.lose_field();
                return;
            }
            let Some((text, rect)) = (unsafe { read_editable(&el, &app_name) }) else {
                self.lose_field();
                return;
            };

            let same = match &self.field {
                Some(f) => unsafe { self.uia.CompareElements(&f.element, &el) }.map(|b| b.as_bool()).unwrap_or(false),
                None => false,
            };
            if same {
                let f = self.field.as_mut().expect("champ");
                if f.text != text {
                    f.text = text;
                    f.changed_at = Instant::now();
                }
                f.rect = rect;
                f.hwnd = fg;
            } else {
                if self.expanded {
                    self.set_expanded(false);
                }
                self.field = Some(Field {
                    element: el,
                    hwnd: fg,
                    app: app_name,
                    text,
                    rect,
                    checked_text: None,
                    // Un champ déjà rempli est vérifié rapidement.
                    changed_at: ago(DEBOUNCE),
                    issues: Vec::new(),
                    error: None,
                });
            }

            let (too_short, needs_check, quiet) = {
                let f = self.field.as_ref().expect("champ");
                let too_short = word_count(&f.text) < MIN_WORDS || f.text.encode_utf16().count() > MAX_UTF16;
                let needs = f.checked_text.as_deref() != Some(f.text.as_str());
                (too_short, needs, f.changed_at.elapsed() >= DEBOUNCE)
            };
            if too_short {
                if let Some(f) = self.field.as_mut() {
                    f.issues.clear();
                    f.checked_text = None;
                }
                self.hide();
                return;
            }
            if needs_check && quiet && self.may_request(&cfg) {
                self.check(&cfg);
            }
            if !self.expanded {
                self.place_bubble();
            }
            self.emit();
        }

        fn may_request(&self, cfg: &Config) -> bool {
            // Le serveur public accepte ~20 requêtes/minute ; un serveur perso n'a pas de limite.
            let min = if cfg.server_url == crate::config::PUBLIC_SERVER {
                Duration::from_millis(3200)
            } else {
                Duration::from_millis(400)
            };
            self.last_request.map_or(true, |t| t.elapsed() >= min)
        }

        fn check(&mut self, cfg: &Config) {
            let Some(text) = self.field.as_ref().map(|f| f.text.clone()) else { return };
            self.last_request = Some(Instant::now());
            let http = self.app.state::<AppState>().http.clone();
            let result = tauri::async_runtime::block_on(langtool::check(&http, cfg, &text, ""));
            let ignored = &self.ignored;
            let next_key = &mut self.next_key;
            let Some(f) = self.field.as_mut() else { return };
            match result {
                Ok(r) => {
                    f.issues = r
                        .issues
                        .into_iter()
                        .filter(|i| !ignored.contains(&(i.rule_id.clone(), i.original.clone())))
                        .map(|mut i| {
                            *next_key += 1;
                            i.key = *next_key;
                            i
                        })
                        .collect();
                    f.error = None;
                    f.checked_text = Some(text);
                }
                Err(e) => {
                    f.issues.clear();
                    f.error = Some(e);
                    f.checked_text = None;
                    f.changed_at = Instant::now() + RETRY_AFTER_ERROR;
                }
            }
        }

        fn scale_at(&self, x: i32, y: i32) -> f64 {
            self.app
                .monitor_from_point(x as f64, y as f64)
                .ok()
                .flatten()
                .map(|m| m.scale_factor())
                .unwrap_or(1.0)
        }

        fn place_bubble(&mut self) {
            let Some(f) = self.field.as_ref() else { return };
            let Some(win) = self.window() else { return };
            let r = f.rect;
            let scale = self.scale_at(r.right, r.top);
            let size = (BUBBLE_SIZE * scale).round() as i32;
            let margin = (4.0 * scale).round() as i32;
            let height = r.bottom - r.top;
            let x = (r.right - size - margin).max(r.left);
            // Grand champ : la bulle suit la ligne du curseur, sur le bord droit (elle ne masque pas la frappe).
            let mut y = if height < size + 2 * margin { r.top + (height - size) / 2 } else { r.bottom - size - margin };
            if height > (90.0 * scale) as i32 {
                if let Some(c) = caret_rect(f.hwnd) {
                    if c.top >= r.top && c.bottom <= r.bottom {
                        y = ((c.top + c.bottom) / 2 - size / 2).clamp(r.top, r.bottom - size);
                    }
                }
            }
            if self.last_pos != Some((x, y)) || !self.visible {
                place_window(&win, x, y, size, size);
                self.last_pos = Some((x, y));
                self.visible = true;
            }
        }

        fn set_expanded(&mut self, expanded: bool) {
            self.expanded = expanded && self.field.is_some();
            let Some(win) = self.window() else { return };
            if self.expanded {
                let (bx, by) = self.last_pos.unwrap_or((100, 100));
                let scale = self.scale_at(bx, by);
                let size = (BUBBLE_SIZE * scale).round() as i32;
                let (w, h) = ((PANEL_W * scale).round() as i32, (PANEL_H * scale).round() as i32);
                let (mut x, mut y) = (bx + size - w, by + size + 4);
                if let Ok(Some(m)) = self.app.monitor_from_point(bx as f64, by as f64) {
                    let area = m.work_area();
                    let (ax, ay) = (area.position.x, area.position.y);
                    let (aw, ah) = (area.size.width as i32, area.size.height as i32);
                    if y + h > ay + ah - 8 {
                        y = by - h - 4;
                    }
                    x = x.clamp(ax + 8, (ax + aw - w - 8).max(ax + 8));
                    y = y.clamp(ay + 8, (ay + ah - h - 8).max(ay + 8));
                }
                place_window(&win, x, y, w, h);
                self.visible = true;
            } else {
                // La pastille sera replacée (et redimensionnée) au prochain passage.
                self.last_pos = None;
            }
        }

        fn state(&self) -> Option<BubbleState> {
            let f = self.field.as_ref()?;
            let stale = f.checked_text.as_deref() != Some(f.text.as_str());
            let status = if f.error.is_some() {
                "error"
            } else if f.checked_text.is_none() {
                "checking"
            } else if f.issues.is_empty() {
                "ok"
            } else {
                "issues"
            };
            Some(BubbleState {
                status: status.into(),
                issues: f.issues.clone(),
                error: f.error.clone(),
                message: self.message.as_ref().filter(|(_, t)| t.elapsed() < Duration::from_secs(6)).map(|(m, _)| m.clone()),
                expanded: self.expanded,
                stale,
                app: f.app.clone(),
            })
        }

        fn emit(&mut self) {
            let Some(state) = self.state() else { return };
            if self.last_emitted.as_ref() == Some(&state) {
                return;
            }
            *self.app.state::<AppState>().bubble_state.lock().unwrap() = Some(state.clone());
            let _ = self.app.emit_to("bubble", "bubble-state", &state);
            self.last_emitted = Some(state);
        }

        fn handle(&mut self, cmd: Command) {
            match cmd {
                Command::Expand(e) => self.set_expanded(e),
                Command::Ignore { rule_id, original } => {
                    if let Some(f) = self.field.as_mut() {
                        f.issues.retain(|i| !(i.rule_id == rule_id && i.original == original));
                    }
                    self.ignored.insert((rule_id, original));
                }
                Command::Recheck => {
                    if let Some(f) = self.field.as_mut() {
                        f.checked_text = None;
                        f.changed_at = ago(DEBOUNCE);
                    }
                    self.last_request = None;
                }
                Command::DropRule(rule_id) => {
                    if let Some(f) = self.field.as_mut() {
                        f.issues.retain(|i| i.rule_id != rule_id);
                    }
                }
                Command::DropWord(word) => {
                    if let Some(f) = self.field.as_mut() {
                        f.issues.retain(|i| !(i.kind == "orthographe" && i.original.eq_ignore_ascii_case(&word)));
                    }
                }
                Command::Apply { key, replacement } => {
                    let target = self
                        .field
                        .as_ref()
                        .and_then(|f| f.issues.iter().find(|i| i.key == key))
                        .map(|i| (i.offset, i.length, i.original.clone()));
                    let Some((offset, length, original)) = target else {
                        self.emit();
                        return;
                    };
                    match self.apply(offset, length, &original, &replacement) {
                        Ok(before) => {
                            let Some(f) = self.field.as_mut() else { return };
                            let repl_len = replacement.encode_utf16().count();
                            f.issues = rebase_issues(std::mem::take(&mut f.issues), offset, length, repl_len);
                            if let Some(after) = splice_utf16(&before, offset, length, &replacement) {
                                // Le texte corrigé hérite de l'analyse de la phrase d'origine :
                                // il est considéré comme vérifié, aucune nouvelle requête n'est lancée.
                                if f.checked_text.as_deref() == Some(before.as_str()) {
                                    f.checked_text = Some(after.clone());
                                }
                                f.text = after;
                            }
                            if f.issues.is_empty() {
                                // Toutes les corrections sont faites : une dernière vérification
                                // confirme le résultat (ou signale une faute apparue entre-temps).
                                f.checked_text = None;
                                f.changed_at = ago(DEBOUNCE - Duration::from_millis(300));
                            }
                        }
                        Err((msg, stale)) => {
                            if stale {
                                if let Some(f) = self.field.as_mut() {
                                    f.checked_text = None;
                                    f.changed_at = ago(DEBOUNCE);
                                }
                            }
                            self.message = Some((msg, Instant::now()));
                        }
                    }
                }
                Command::OpenEditor => {
                    if let Some(f) = self.field.as_ref() {
                        let (text, hwnd) = (f.text.clone(), f.hwnd);
                        self.set_expanded(false);
                        self.hide();
                        crate::open_editor_with_field(&self.app, text, hwnd);
                    }
                }
            }
            self.emit();
        }

        /// Corrige le mot dans le champ. Renvoie le texte du champ juste avant la correction.
        /// En cas d'échec : (message, faut-il revérifier le texte ?).
        fn apply(&mut self, offset: usize, length: usize, original: &str, replacement: &str) -> Result<String, (String, bool)> {
            let f = self.field.as_ref().ok_or(("Le champ n'est plus actif.".to_string(), false))?;
            let (el, hwnd) = (f.element.clone(), f.hwnd);
            let fail = |msg: &str| (msg.to_string(), false);
            let fail_e = |e: windows::core::Error| (e.to_string(), false);
            unsafe {
                let current = read_text(&el).ok_or_else(|| fail("Impossible de relire le texte du champ."))?;
                let units: Vec<u16> = current.encode_utf16().collect();
                if langtool::utf16_slice(&units, offset, length).as_deref() != Some(original) {
                    return Err(("Le texte a changé depuis la vérification : nouvelle vérification en cours.".into(), true));
                }
                SetForegroundWindow(hwnd as _);
                let _ = el.SetFocus();
                sleep(Duration::from_millis(60));

                if let Some(tp) = text_pattern(&el) {
                    let doc = tp.DocumentRange().map_err(fail_e)?;
                    let caret = text_before_caret(&tp, &doc).map(|t| t.encode_utf16().count());
                    let prefix = String::from_utf16_lossy(&units[..offset]);
                    // Selon l'application, un « caractère » UI Automation n'a pas la même taille :
                    // on essaie plusieurs conventions et on garde celle qui retombe sur le bon mot.
                    let mut chosen = None;
                    for u in [Units::Utf16, Units::Scalar, Units::CrLfAsOne] {
                        let (start, len) = (count_units(&prefix, u), count_units(original, u));
                        if let Some(r) = range_at(&doc, start, len) {
                            if r.GetText(-1).map(|t| t.to_string()).ok().as_deref() == Some(original) {
                                r.Select().map_err(fail_e)?;
                                chosen = Some(u);
                                break;
                            }
                        }
                    }
                    let Some(u) = chosen else {
                        return Err(fail("Impossible de cibler ce mot dans cette application : utilise « Ouvrir dans l'éditeur »."));
                    };
                    sleep(Duration::from_millis(40));
                    type_text(replacement).map_err(|e| (e, false))?;
                    // On remet le curseur là où l'utilisateur écrivait.
                    if let Some(c) = caret {
                        let new_caret = shift_caret(c, offset, length, replacement.encode_utf16().count());
                        sleep(Duration::from_millis(60));
                        if let (Some(new_text), Ok(doc)) = (read_text(&el), tp.DocumentRange()) {
                            let nu: Vec<u16> = new_text.encode_utf16().collect();
                            let pre = String::from_utf16_lossy(&nu[..new_caret.min(nu.len())]);
                            if let Some(r) = range_at(&doc, count_units(&pre, u), 0) {
                                let _ = r.Select();
                            }
                        }
                    }
                    Ok(current)
                } else if let Some(vp) = value_pattern(&el) {
                    let new_text = splice_utf16(&current, offset, length, replacement).ok_or_else(|| fail("Position invalide."))?;
                    vp.SetValue(&BSTR::from(new_text.as_str())).map_err(fail_e)?;
                    Ok(current)
                } else {
                    Err(fail("Ce champ ne permet pas la correction directe : utilise « Ouvrir dans l'éditeur »."))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_and_units() {
        assert_eq!(word_count("youtube.com"), 1);
        assert_eq!(word_count("Bonjoru les amis"), 3);
        assert_eq!(word_count("  a  -  b "), 2);
        let s = "é\r\n😀x";
        assert_eq!(count_units(s, Units::Utf16), 6);
        assert_eq!(count_units(s, Units::Scalar), 5);
        assert_eq!(count_units(s, Units::CrLfAsOne), 5);
    }

    fn issue(offset: usize, length: usize, original: &str, key: u64) -> Issue {
        Issue {
            offset,
            length,
            original: original.into(),
            message: String::new(),
            short_message: String::new(),
            replacements: vec![],
            rule_id: "R".into(),
            rule_description: String::new(),
            category: String::new(),
            kind: "orthographe".into(),
            key,
        }
    }

    #[test]
    fn rebase_keeps_original_analysis() {
        // "Bonjoru les amis est la ?" : on corrige « est » -> « sont » (+1 unité)
        let issues = vec![issue(0, 7, "Bonjoru", 1), issue(17, 3, "est", 2), issue(23, 2, " ?", 3)];
        let after = rebase_issues(issues, 17, 3, 4);
        let got: Vec<(u64, usize)> = after.iter().map(|i| (i.key, i.offset)).collect();
        assert_eq!(got, vec![(1, 0), (3, 24)]);
        let text = splice_utf16("Bonjoru les amis est la ?", 17, 3, "sont").unwrap();
        let units: Vec<u16> = text.encode_utf16().collect();
        assert_eq!(crate::langtool::utf16_slice(&units, 24, 2).unwrap(), " ?");
        // puis « Bonjoru » -> « Bonjour » (même longueur) : rien ne bouge
        let after = rebase_issues(after, 0, 7, 7);
        assert_eq!(after.iter().map(|i| (i.key, i.offset)).collect::<Vec<_>>(), vec![(3, 24)]);
    }

    #[test]
    fn splice_and_caret() {
        assert_eq!(splice_utf16("Les amis est là", 9, 3, "sont").unwrap(), "Les amis sont là");
        assert_eq!(splice_utf16("😀 ab", 3, 2, "cd").unwrap(), "😀 cd");
        assert!(splice_utf16("abc", 2, 5, "x").is_none());
        // curseur après la faute : décalé ; avant : inchangé ; dedans : fin du remplacement
        assert_eq!(shift_caret(20, 9, 3, 4), 21);
        assert_eq!(shift_caret(5, 9, 3, 4), 5);
        assert_eq!(shift_caret(10, 9, 3, 4), 13);
    }
}
