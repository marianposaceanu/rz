use super::*;
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;
use tempfile::TempDir;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn fixture() -> (TempDir, App, Manager) {
    let temp = TempDir::new().unwrap();
    let app = App::for_test(temp.path());
    for (id, saved, agent, bytes) in [
        ("old", "2020-01-01T10:00:00Z".to_owned(), "pi", 64_000),
        ("new", Local::now().to_rfc3339(), "codex", 100),
        ("middle", "2025-01-01T10:00:00Z".to_owned(), "claude", 100),
    ] {
        let mut pane = json!({"index": 1, "name": "π - project", "focused": true,
            "working_directory": "/tmp/π-project", "scrollback_file": "scrollback/history.txt"});
        pane[format!("{agent}_session_id")] = json!(format!("{agent}-session"));
        let value = json!({"version": 1, "id": id, "name": id, "saved_at": saved,
            "rz_version": env!("CARGO_PKG_VERSION"), "scrollback_captured": true,
            "windows": [{"index": 1, "tabs": [{"index": 1, "name": "Project",
                "layout": {"type":"split", "direction":"horizontal", "first_size":40, "total_size":100,
                    "first":{"type":"leaf", "terminal":1}, "second":{"type":"leaf", "terminal":2}},
                "terminals": [pane, {"index": 2, "name": "shell", "working_directory": "/tmp/shell"}]}]}]});
        let directory = app.snapshots.root.join(id);
        storage::write_json(&directory.join("state.json"), &value).unwrap();
        fs::create_dir(directory.join("scrollback")).unwrap();
        fs::write(directory.join("scrollback/history.txt"), vec![b'x'; bytes]).unwrap();
    }
    let manager = Manager::new(Catalog::load(&app.snapshots).unwrap());
    (temp, app, manager)
}

use std::fs;

#[test]
fn filters_by_agent_and_directory_and_wraps_selection() {
    let (_temp, _app, mut manager) = fixture();
    assert_eq!(manager.selected().unwrap().entry.id(), "new");
    manager.handle_key(key(KeyCode::Up));
    assert_eq!(manager.selected().unwrap().entry.id(), "old");
    manager.handle_key(key(KeyCode::Down));
    assert_eq!(manager.selected().unwrap().entry.id(), "new");
    manager.query = "pi-session".into();
    manager.rebuild(None);
    assert_eq!(manager.visible.len(), 1);
    assert_eq!(manager.selected().unwrap().entry.id(), "old");
    manager.query = "π-project".into();
    manager.rebuild(None);
    assert_eq!(manager.visible.len(), 3);
    manager.query = "no matches".into();
    manager.rebuild(None);
    assert!(manager.selected().is_none());
    assert!(matches!(
        manager.handle_key(key(KeyCode::Enter)),
        Effect::None
    ));
}

#[test]
fn storage_sorts_by_disk_usage_and_refresh_preserves_selection() {
    let (_temp, app, mut manager) = fixture();
    manager.handle_key(key(KeyCode::Tab));
    assert!(manager.page == Page::Storage);
    assert_eq!(manager.selected().unwrap().entry.id(), "old");
    manager.complete(Completed {
        message: Ok("Refreshed".into()),
        saved_name: None,
        catalog: Ok(Catalog::load(&app.snapshots).unwrap()),
    });
    assert_eq!(manager.selected().unwrap().entry.id(), "old");
}

#[test]
fn restore_requires_confirmation_and_keeps_windows_by_default() {
    let (_temp, _app, mut manager) = fixture();
    assert!(matches!(
        manager.handle_key(key(KeyCode::Enter)),
        Effect::None
    ));
    assert!(matches!(manager.modal, Some(Modal::Confirm { .. })));
    assert!(matches!(
        manager.handle_key(key(KeyCode::Esc)),
        Effect::None
    ));
    assert!(manager.modal.is_none());
    manager.handle_key(key(KeyCode::Enter));
    assert!(
        matches!(manager.handle_key(key(KeyCode::Char('y'))), Effect::Restore(options) if !options.close_existing && options.selector.as_deref() == Some("new"))
    );
    manager.handle_key(key(KeyCode::Char('R')));
    assert!(
        matches!(manager.handle_key(key(KeyCode::Enter)), Effect::Restore(options) if options.close_existing)
    );
}

#[test]
fn deletion_is_previewed_and_cancellation_preserves_files() {
    let (_temp, app, mut manager) = fixture();
    manager.handle_key(key(KeyCode::Char('d')));
    assert!(
        matches!(manager.modal, Some(Modal::Confirm { ref lines, .. }) if lines.iter().any(|line| line.contains("Frees")))
    );
    manager.handle_key(key(KeyCode::Char('n')));
    assert!(app.snapshots.root.join("new/state.json").is_file());
    manager.handle_key(key(KeyCode::Char('d')));
    let Effect::Work(job) = manager.handle_key(key(KeyCode::Char('y'))) else {
        panic!("expected confirmed deletion")
    };
    execute(&app, job).unwrap();
    assert!(!app.snapshots.root.join("new").exists());
    assert!(app.snapshots.root.join("old/state.json").is_file());
}

#[test]
fn cleanup_ignores_search_and_freezes_only_previewed_paths() {
    let (_temp, app, mut manager) = fixture();
    manager.query = "new".into();
    manager.rebuild(None);
    assert_eq!(manager.visible.len(), 1);
    manager.prepare_cleanup("7.days").unwrap();
    let Effect::Work(Job::Delete(paths)) = manager.handle_key(key(KeyCode::Char('y'))) else {
        panic!("expected cleanup")
    };
    assert_eq!(paths.len(), 2);
    assert!(!paths.contains(&app.snapshots.root.join("new")));
    assert!(paths.contains(&app.snapshots.root.join("old")));
    assert!(manager.prepare_cleanup("0").is_err());
}

#[test]
fn invalid_save_name_keeps_prompt_open_and_busy_state_prevents_actions() {
    let (_temp, _app, mut manager) = fixture();
    manager.modal = Some(Modal::Input {
        prompt: Prompt::Save,
        value: "../outside".into(),
    });
    assert!(matches!(
        manager.handle_key(key(KeyCode::Enter)),
        Effect::None
    ));
    assert!(manager.error && manager.modal.is_some());
    let prompt = render(&mut manager, 64, 20);
    assert!(prompt.contains("invalid snapshot name") && prompt.contains("Enter continue"));
    manager.handle_key(key(KeyCode::Esc));
    manager.busy = true;
    for code in [
        KeyCode::Char('q'),
        KeyCode::Char('d'),
        KeyCode::Char('s'),
        KeyCode::Enter,
    ] {
        assert!(matches!(manager.handle_key(key(code)), Effect::None));
    }
    assert!(manager.quit_after_job);
    let selected = manager.selected().unwrap().entry.path.clone();
    manager.handle_key(key(KeyCode::Down));
    assert_ne!(manager.selected().unwrap().entry.path, selected);
}

#[test]
fn small_terminals_block_actions_and_confirmation_keys_remain_visible() {
    let (_temp, _app, mut manager) = fixture();
    assert!(render(&mut manager, 30, 10).contains("Resize to at least"));
    assert!(matches!(
        manager.handle_key(key(KeyCode::Char('d'))),
        Effect::None
    ));
    assert!(manager.modal.is_none());
    render(&mut manager, 64, 20);
    manager.prepare_cleanup("7d").unwrap();
    let preview = render(&mut manager, 64, 20);
    assert!(preview.contains("snapshots older than") && preview.contains("y / Enter confirm"));
}

#[test]
fn successful_save_selects_new_snapshot_even_with_an_active_search() {
    let (_temp, app, mut manager) = fixture();
    manager.query = "old".into();
    manager.rebuild(None);
    manager.complete(Completed {
        message: Ok("Saved new.".into()),
        saved_name: Some("new".into()),
        catalog: Ok(Catalog::load(&app.snapshots).unwrap()),
    });
    assert!(manager.query.is_empty());
    assert_eq!(manager.selected().unwrap().entry.id(), "new");
}

fn render(manager: &mut Manager, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| manager.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn renders_layout_storage_empty_state_and_unicode_at_multiple_sizes() {
    let (_temp, _app, mut manager) = fixture();
    let wide = render(&mut manager, 140, 40);
    assert!(
        wide.contains("Layout preview") && wide.contains("Codex") && wide.contains("/tmp/shell")
    );
    manager.handle_key(key(KeyCode::Tab));
    let storage = render(&mut manager, 140, 40);
    assert!(storage.contains("legacy plain-text") && storage.contains("Share of snapshot storage"));
    for (width, height) in [(80, 24), (64, 20), (20, 8)] {
        render(&mut manager, width, height);
    }
    manager.catalog.items.clear();
    manager.rebuild(None);
    assert!(render(&mut manager, 100, 30).contains("No snapshots yet"));
}
