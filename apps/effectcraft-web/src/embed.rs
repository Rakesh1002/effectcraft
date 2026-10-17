//! `?embed`: the app as a component of a host page that owns storage, files and downloads.
//!
//! - storage is memory only and the app starts empty: no session restore, demo project, folder
//!   handles, disk cache or auto-save;
//! - File ▸ Import / Open and dropped files raise `effectcraft:import-request` `{kind, files}`
//!   (the browser `File`s) instead of importing;
//! - saving a project raises `effectcraft:save-request` `{path}` and downloads nothing; other
//!   written files and render outputs stay in the file table and raise `effectcraft:output
//!   {paths}`;
//! - a change of the session's revision or modified state raises `effectcraft:revision
//!   {revision, dirty}`;
//! - the menus, shortcuts and header leave out what the host page owns or a page can't do
//!   ([`HIDDEN`]).
//!
//! Events are dispatched on `window` from a microtask, after the frame, so a listener can call
//! back into `window.effectcraft` right away.
//!
//! A hidden page gets no animation frames, so the control channel (`execute`, `request`,
//! `renderFrame`) would wait for the tab to show again; while hidden, requests are served between
//! frames instead ([`ControlPump`]).

/// Menu entries, shortcuts and header buttons an embedded page leaves out
/// ([`effectcraft_engine::menus::Hidden`]): the host owns the project (new, open, recent, save
/// as, revert, demo, templates, the Home screen); the desktop's windows, folders and file
/// managers; disk cache, scripts and plug-ins; pickers whose file would reach the host as an
/// import instead of the command; files written for a download nobody receives; the upstream
/// project's help and links.
pub const HIDDEN: &[&str] = &[
    "app.hide",
    "app.hideOthers",
    "app.showAll",
    "app.quit",
    "app.home",
    "file.newProject",
    "file.newFromTemplate",
    "file.open",
    "@dynamic:recentProjects",
    "file.openRecent",
    "file.clearRecent",
    "file.closeProject",
    "file.saveAs",
    "file.saveCopy",
    "file.saveCopyAsXml",
    "templates.saveAs",
    "file.incrementAndSave",
    "file.revert",
    "file.importTimeline",
    "file.importProject",
    "file.importLottie",
    "file.importVanishingPoint",
    "essential.importTemplate",
    "@dynamic:recentFootage",
    "file.importRecent",
    "file.clearRecentFootage",
    "file.exportTimeline",
    "file.exportLottie",
    "essential.exportTemplate",
    "file.collectFiles",
    "file.relinkFootage",
    "file.watchFolder",
    "@dynamic:scripts",
    "file.installScript",
    "file.installScriptUIPanel",
    "file.runScript",
    "file.createProxy",
    "file.setProxy",
    "file.setProxyNone",
    "file.replaceFootage",
    "file.revealInFinder",
    "file.openDemoProject",
    r#"edit.purge {"what":"disk"}"#,
    r#"edit.purge {"what":"memoryAndDisk"}"#,
    "edit.editOriginal",
    r#"app.settings {"page":"startup"}"#,
    r#"app.settings {"page":"project"}"#,
    r#"app.settings {"page":"export"}"#,
    r#"app.settings {"page":"disk"}"#,
    r#"app.settings {"page":"memory"}"#,
    r#"app.settings {"page":"video"}"#,
    "comp.saveFrameAs",
    "comp.saveFrameAsPsd",
    "comp.saveFrameAsExr",
    "render.saveCurrentPreview",
    "layer.revealInFinder",
    "effect.plugins.load",
    "anim.savePreset",
    "anim.applyPreset",
    "keys.rpfCameraImport",
    "view.importGuides",
    "view.exportGuides",
    "view.fullScreen",
    r#"window.panel {"panel":"mediaBrowser"}"#,
    r#"window.panel {"panel":"scriptConsole"}"#,
    "@dynamic:scriptPanels",
    "window.scriptPanel",
    r#"window.workspace {"name":"Learn"}"#,
    "help.inAppTutorials",
    "window.maximizeApp",
    "window.panelWindow",
    "help.docs",
    "help.onlineTutorials",
    "help.systemReport",
    "help.enableLogging",
    "help.revealLogFile",
    "help.reportIssue",
    "help.appPage",
    "help.github",
];

/// A project file the host saves (`.ecproj`, or its XML form).
pub fn is_project_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".ecproj") || lower.ends_with(".ecprojx")
}

/// Reports `(revision, dirty)` when it differs from the last reported pair.
#[derive(Default)]
pub struct RevisionWatch {
    last: Option<(u64, bool)>,
}

impl RevisionWatch {
    pub fn changed(&mut self, revision: u64, dirty: bool) -> Option<(u64, bool)> {
        let now = (revision, dirty);
        if self.last == Some(now) {
            return None;
        }
        self.last = Some(now);
        Some(now)
    }
}

/// When to serve control requests between frames: from a microtask right after a request
/// arrives in a hidden page (timers there are throttled to about one a second), then on a timer
/// while replies are still pending (a job running in a worker). At most one of each is queued; a
/// visible page leaves it to the frames.
#[derive(Default)]
pub struct ControlPump {
    queued: bool,
    timer: bool,
}

impl ControlPump {
    /// A request arrived or the page was hidden: whether to queue a pump now.
    pub fn wake(&mut self, embedded: bool, hidden: bool) -> bool {
        if !embedded || !hidden || self.queued {
            return false;
        }
        self.queued = true;
        true
    }

    /// A queued (`timer`: false) or timed pump runs: whether to serve the channel (the page may
    /// have shown again).
    pub fn run(&mut self, timer: bool, hidden: bool) -> bool {
        if timer {
            self.timer = false;
        } else {
            self.queued = false;
        }
        hidden
    }

    /// After serving: whether to set a timer to pump again.
    pub fn again(&mut self, hidden: bool, pending: usize) -> bool {
        if !hidden || pending == 0 || self.timer {
            return false;
        }
        self.timer = true;
        true
    }
}

#[cfg(target_arch = "wasm32")]
pub use web::*;

#[cfg(target_arch = "wasm32")]
mod web {
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, Ordering};

    use effectcraft_engine::Session;
    use serde_json::{Value, json};
    use wasm_bindgen::prelude::*;

    use super::RevisionWatch;

    static EMBED: AtomicBool = AtomicBool::new(false);

    thread_local! {
        static WATCH: RefCell<RevisionWatch> = RefCell::new(RevisionWatch::default());
    }

    pub fn active() -> bool {
        EMBED.load(Ordering::Relaxed)
    }

    pub(crate) fn set_active() {
        EMBED.store(true, Ordering::Relaxed);
    }

    fn dispatch(name: &str, detail: &JsValue) -> Result<(), JsValue> {
        let w = web_sys::window().ok_or("no window")?;
        let init = web_sys::CustomEventInit::new();
        init.set_detail(detail);
        let ev = web_sys::CustomEvent::new_with_event_init_dict(name, &init)?;
        w.dispatch_event(&ev)?;
        Ok(())
    }

    pub fn emit(name: &'static str, detail: JsValue) {
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = dispatch(name, &detail) {
                log::warn!("{name}: {e:?}");
            }
        });
    }

    pub fn emit_json(name: &'static str, detail: &Value) {
        emit(name, js_sys::JSON::parse(&detail.to_string()).unwrap_or(JsValue::NULL));
    }

    /// `kind`: `import` (File ▸ Import), `open` (File ▸ Open Project) or `drop`.
    pub fn import_request(kind: &str, files: Vec<web_sys::File>) {
        if files.is_empty() {
            return;
        }
        let detail = js_sys::Object::new();
        let list: js_sys::Array = files.into_iter().map(JsValue::from).collect();
        let _ = js_sys::Reflect::set(&detail, &"kind".into(), &kind.into());
        let _ = js_sys::Reflect::set(&detail, &"files".into(), &list);
        emit("effectcraft:import-request", detail.into());
    }

    pub fn save_request(path: Option<&str>) {
        emit_json("effectcraft:save-request", &json!({"path": path}));
    }

    pub fn output(paths: Vec<String>) {
        if !paths.is_empty() {
            emit_json("effectcraft:output", &json!({"paths": paths}));
        }
    }

    /// Called once per frame: raises `effectcraft:revision` when the revision or the modified
    /// state changed.
    pub fn tick(s: &Session) {
        if let Some((revision, dirty)) = WATCH.with(|w| w.borrow_mut().changed(s.revision, s.is_dirty())) {
            emit_json("effectcraft:revision", &json!({"revision": revision, "dirty": dirty}));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_rules_name_menu_entries_and_keep_the_editing_menus() {
        use effectcraft_engine::menus::{self, Hidden, MenuNode};
        // UI commands (ui-egui's `UI_COMMANDS`, not built natively here).
        const UI_ONLY: [&str; 3] = ["app.home", "window.maximizeApp", "window.panelWindow"];
        let hidden = Hidden::new(HIDDEN).unwrap();
        let entries = menus::entries();
        for rule in HIDDEN {
            let id = rule.split(' ').next().unwrap();
            if let Some(name) = id.strip_prefix("@dynamic:") {
                assert!(menus::TREE.contains(&format!("@dynamic:{name}")), "{rule}");
            } else if rule.contains(' ') {
                assert!(entries.iter().any(|(_, e)| e.command == id && hidden.command(&e.command, &e.params)), "{rule} matches no menu entry");
            } else {
                assert!(effectcraft_engine::commands::find(id).is_some() || UI_ONLY.contains(&id), "{rule}");
            }
        }
        // The web's menus are the non-macOS tree.
        let web = menus::parse(menus::TREE, false).unwrap();
        let shown = |path: &[&str]| -> Vec<String> {
            let mut nodes = &web[..];
            for label in path {
                let Some(MenuNode::Submenu { children, .. }) = nodes.iter().find(|n| matches!(n, MenuNode::Submenu { label: l, .. } if l == label)) else {
                    return vec![];
                };
                nodes = children;
            }
            hidden
                .shown(nodes)
                .into_iter()
                .map(|n| match n {
                    MenuNode::Item(e) => e.command.clone(),
                    MenuNode::Submenu { label, .. } => label.clone(),
                    MenuNode::Separator => "---".into(),
                    MenuNode::Dynamic { name } => format!("@{name}"),
                })
                .collect()
        };
        let file = shown(&["File"]);
        for kept in ["New", "file.close", "file.save", "Import", "Export", "Interpret Footage", "Replace Footage", "file.projectSettings"] {
            assert!(file.contains(&kept.to_string()), "File keeps {kept}: {file:?}");
        }
        for gone in ["Open Recent", "Save As", "Scripts", "Create Proxy", "Set Proxy", "Import Recent Footage", "Watch Folder"] {
            assert!(!file.contains(&gone.to_string()), "File drops {gone}: {file:?}");
        }
        assert_eq!(shown(&["File", "New"]), ["project.newFolder"]);
        assert_eq!(shown(&["File", "Import"]), ["file.import", "file.importMultiple", "file.importPlaceholder", "file.importSolid"]);
        assert_eq!(shown(&["File", "Export"]), ["renderQueue.add"]);
        assert!(!shown(&["Composition"]).contains(&"Save Frame As".to_string()));
        let help = shown(&["Help"]);
        assert!(help.contains(&"app.about".to_string()) && !help.contains(&"help.github".to_string()), "{help:?}");
        for menu in ["File", "Edit", "Composition", "Layer", "Effect", "Animation", "View", "Window", "Help"] {
            let items = shown(&[menu]);
            assert!(items.first().is_some_and(|f| f != "---") && items.last().is_some_and(|l| l != "---"), "{menu}: {items:?}");
            assert!(!items.windows(2).any(|w| w[0] == "---" && w[1] == "---"), "{menu}: {items:?}");
        }
    }

    #[test]
    fn project_paths_are_the_host_saves() {
        assert!(is_project_path("/Intro.ecproj"));
        assert!(is_project_path("/a/B.ECPROJX"));
        assert!(!is_project_path("/Frame.png"));
        assert!(!is_project_path("/notes.ecproj.txt"));
    }

    #[test]
    fn the_pump_serves_hidden_embedded_pages_only() {
        let mut p = ControlPump::default();
        assert!(!p.wake(false, true));
        assert!(!p.wake(true, false));
        assert!(p.wake(true, true));
        assert!(!p.wake(true, true), "one queued pump at a time");
        assert!(p.run(false, true));
        assert!(p.again(true, 1), "replies still pending");
        assert!(!p.again(true, 1), "one timer at a time");
        assert!(p.wake(true, true), "a new request does not wait for the timer");
        assert!(p.run(false, true));
        assert!(!p.run(true, false), "the page showed again: frames serve");
        assert!(!p.again(false, 1));
        assert!(p.wake(true, true));
        assert!(p.run(false, true));
        assert!(!p.again(true, 0), "nothing pending");
    }

    #[test]
    fn revision_watch_reports_each_change_once() {
        let mut w = RevisionWatch::default();
        assert_eq!(w.changed(0, false), Some((0, false)));
        assert_eq!(w.changed(0, false), None);
        assert_eq!(w.changed(3, true), Some((3, true)));
        assert_eq!(w.changed(3, true), None);
        assert_eq!(w.changed(3, false), Some((3, false)));
    }
}
