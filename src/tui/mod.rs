mod view;

use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    widgets::TableState,
};

use crate::{
    App,
    app::validate_name,
    cli::{self, RestoreOptions, SaveOptions},
    storage::{self, Usage},
    store::{SnapshotEntry, SnapshotStore},
};

pub fn run(app: &App) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!(
            "the workspace manager needs an interactive terminal; use rz --save NAME or rz --restore for direct commands"
        );
    }
    let mut manager = Manager::new(Catalog::load(&app.snapshots)?);
    let restore = ratatui::run(|terminal| manager.run(terminal, app))?;
    if let Some(options) = restore {
        app.restore(options)?;
    }
    Ok(())
}

struct Item {
    entry: SnapshotEntry,
    usage: Result<Usage, String>,
    search: String,
}

struct Catalog {
    items: Vec<Item>,
    usage: Usage,
    root: PathBuf,
}

impl Catalog {
    fn load(store: &SnapshotStore) -> Result<Self> {
        let items = store
            .entries()?
            .into_iter()
            .rev()
            .map(|entry| {
                let mut search = entry.id().to_owned();
                if let Ok(snapshot) = entry.snapshot() {
                    search.push_str(&format!(" {} {}", snapshot.name, snapshot.saved_at));
                    for terminal in snapshot.terminals() {
                        search.push_str(&format!(
                            " {} {}",
                            terminal.name, terminal.working_directory
                        ));
                        if let Some(session) = terminal.session() {
                            search.push_str(&format!(" {} {}", session.agent, session.id));
                        }
                    }
                }
                let usage = storage::usage(&entry.path).map_err(|error| format!("{error:#}"));
                Item {
                    entry,
                    usage,
                    search: search.to_lowercase(),
                }
            })
            .collect();
        Ok(Self {
            items,
            usage: store.usage()?,
            root: store.root.clone(),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Workspaces,
    Storage,
}

#[derive(Clone, Copy)]
enum Sort {
    Newest,
    Largest,
    Name,
}

impl Sort {
    fn label(self) -> &'static str {
        match self {
            Self::Newest => "Newest",
            Self::Largest => "Largest",
            Self::Name => "Name",
        }
    }
}

#[derive(Clone, Copy)]
enum Prompt {
    Save,
    Clean,
}

enum Modal {
    Input {
        prompt: Prompt,
        value: String,
    },
    Confirm {
        title: String,
        lines: Vec<String>,
        effect: Effect,
    },
}

#[derive(Debug)]
enum Job {
    Refresh,
    Save(String),
    Delete(Vec<PathBuf>),
}

#[derive(Debug)]
enum Effect {
    None,
    Quit,
    Work(Job),
    Restore(RestoreOptions),
}

struct Completed {
    message: Result<String, String>,
    saved_name: Option<String>,
    catalog: Result<Catalog, String>,
}

struct Manager {
    catalog: Catalog,
    visible: Vec<usize>,
    table: TableState,
    page: Page,
    sort: Sort,
    query: String,
    searching: bool,
    modal: Option<Modal>,
    notice: String,
    error: bool,
    busy: bool,
    quit_after_job: bool,
    too_small: bool,
    detail_tab: usize,
    detail_scroll: u16,
}

impl Manager {
    fn new(catalog: Catalog) -> Self {
        let mut manager = Self {
            catalog,
            visible: Vec::new(),
            table: TableState::default(),
            page: Page::Workspaces,
            sort: Sort::Newest,
            query: String::new(),
            searching: false,
            modal: None,
            notice: "Enter: restore selected snapshot · s: save a new workspace".into(),
            error: false,
            busy: false,
            quit_after_job: false,
            too_small: false,
            detail_tab: 0,
            detail_scroll: 0,
        };
        manager.rebuild(None);
        manager
    }

    fn run(&mut self, terminal: &mut DefaultTerminal, app: &App) -> Result<Option<RestoreOptions>> {
        let (sender, receiver) = mpsc::channel();
        loop {
            if let Ok(completed) = receiver.try_recv() {
                self.complete(completed);
                if self.quit_after_job {
                    return Ok(None);
                }
            }
            terminal.draw(|frame| self.draw(frame))?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match self.handle_key(key) {
                Effect::None => {}
                Effect::Quit => return Ok(None),
                Effect::Restore(options) => return Ok(Some(options)),
                Effect::Work(job) => {
                    self.busy = true;
                    self.set_notice(
                        match &job {
                            Job::Refresh => "Refreshing snapshots…",
                            Job::Save(_) => "Saving exact layouts and LZ4 scrollback…",
                            Job::Delete(_) => "Removing the confirmed snapshots…",
                        },
                        false,
                    );
                    let app = app.clone();
                    let sender = sender.clone();
                    thread::spawn(move || {
                        let saved_name = match &job {
                            Job::Save(name) => Some(name.clone()),
                            _ => None,
                        };
                        let message = execute(&app, job).map_err(|error| format!("{error:#}"));
                        let catalog =
                            Catalog::load(&app.snapshots).map_err(|error| format!("{error:#}"));
                        let _ = sender.send(Completed {
                            message,
                            saved_name,
                            catalog,
                        });
                    });
                }
            }
        }
    }

    fn selected(&self) -> Option<&Item> {
        self.table
            .selected()
            .and_then(|index| self.visible.get(index))
            .map(|index| &self.catalog.items[*index])
    }

    fn rebuild(&mut self, selected: Option<PathBuf>) {
        let query = self.query.to_lowercase();
        self.visible = self
            .catalog
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.search.contains(&query))
            .map(|(index, _)| index)
            .collect();
        match self.sort {
            Sort::Newest => {} // Catalog order is newest first.
            Sort::Largest => self.visible.sort_by_key(|index| {
                std::cmp::Reverse(
                    self.catalog.items[*index]
                        .usage
                        .as_ref()
                        .map_or(0, |usage| usage.allocated),
                )
            }),
            Sort::Name => self.visible.sort_by(|a, b| {
                self.catalog.items[*a]
                    .entry
                    .id()
                    .cmp(self.catalog.items[*b].entry.id())
            }),
        }
        let index = selected
            .and_then(|path| {
                self.visible
                    .iter()
                    .position(|index| self.catalog.items[*index].entry.path == path)
            })
            .unwrap_or(0);
        self.table
            .select((!self.visible.is_empty()).then_some(index));
        self.detail_tab = 0;
        self.detail_scroll = 0;
    }

    fn complete(&mut self, completed: Completed) {
        let selected = self.selected().map(|item| item.entry.path.clone());
        self.busy = false;
        let message = match completed.catalog {
            Ok(catalog) => {
                let created = completed
                    .saved_name
                    .as_ref()
                    .and_then(|name| {
                        catalog.items.iter().find(|item| {
                            item.entry
                                .snapshot()
                                .is_ok_and(|snapshot| &snapshot.name == name)
                        })
                    })
                    .map(|item| item.entry.path.clone());
                if created.is_some() {
                    self.query.clear();
                }
                self.catalog = catalog;
                self.rebuild(created.or(selected));
                completed.message
            }
            Err(error) => Err(format!(
                "{}; refresh failed: {error}",
                completed.message.unwrap_or_else(|error| error)
            )),
        };
        match message {
            Ok(message) => self.set_notice(message, false),
            Err(error) => self.set_notice(error, true),
        }
    }

    fn set_notice(&mut self, message: impl Into<String>, error: bool) {
        self.notice = message.into();
        self.error = error;
    }

    fn handle_key(&mut self, key: KeyEvent) -> Effect {
        let interrupt =
            key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
        if self.busy && (interrupt || (!self.searching && key.code == KeyCode::Char('q'))) {
            self.quit_after_job = true;
            self.set_notice("Finishing the current operation before closing…", false);
            return Effect::None;
        }
        if self.busy
            && !self.searching
            && !matches!(
                key.code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::PageDown
                    | KeyCode::PageUp
                    | KeyCode::Tab
                    | KeyCode::Esc
                    | KeyCode::Char('j' | 'k' | 'o' | '/' | '[' | ']')
            )
        {
            return Effect::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Effect::Quit;
        }
        if self.too_small {
            return if key.code == KeyCode::Char('q') {
                Effect::Quit
            } else {
                Effect::None
            };
        }
        if let Some(modal) = self.modal.take() {
            return self.handle_modal(modal, key);
        }
        if self.searching {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.searching = false,
                KeyCode::Backspace => {
                    self.query.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.query.clear()
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.query.push(character)
                }
                _ => {}
            }
            self.rebuild(None);
            return Effect::None;
        }
        match key.code {
            KeyCode::Char('q') => return Effect::Quit,
            KeyCode::Esc => {
                self.query.clear();
                self.rebuild(None);
            }
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Home => {
                self.table.select((!self.visible.is_empty()).then_some(0));
                self.detail_tab = 0;
                self.detail_scroll = 0;
            }
            KeyCode::End => {
                self.table.select(self.visible.len().checked_sub(1));
                self.detail_tab = 0;
                self.detail_scroll = 0;
            }
            KeyCode::PageDown => self.detail_scroll = self.detail_scroll.saturating_add(5),
            KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(5),
            KeyCode::Left | KeyCode::Char('[') => self.move_tab(-1),
            KeyCode::Right | KeyCode::Char(']') => self.move_tab(1),
            KeyCode::Tab => {
                self.page = if self.page == Page::Workspaces {
                    Page::Storage
                } else {
                    Page::Workspaces
                };
                self.sort = if self.page == Page::Storage {
                    Sort::Largest
                } else {
                    Sort::Newest
                };
                self.rebuild(None);
            }
            KeyCode::Char('o') => {
                self.sort = match self.sort {
                    Sort::Newest => Sort::Largest,
                    Sort::Largest => Sort::Name,
                    Sort::Name => Sort::Newest,
                };
                self.rebuild(self.selected().map(|item| item.entry.path.clone()));
            }
            KeyCode::Char('r') => return Effect::Work(Job::Refresh),
            KeyCode::Char('s') => {
                self.error = false;
                self.modal = Some(Modal::Input {
                    prompt: Prompt::Save,
                    value: String::new(),
                })
            }
            KeyCode::Char('c') => {
                self.error = false;
                self.modal = Some(Modal::Input {
                    prompt: Prompt::Clean,
                    value: "7d".into(),
                })
            }
            KeyCode::Char('d') => self.confirm_delete(),
            KeyCode::Enter => self.confirm_restore(false),
            KeyCode::Char('R') => self.confirm_restore(true),
            _ => {}
        }
        Effect::None
    }

    fn move_selection(&mut self, delta: isize) {
        let length = self.visible.len();
        if length > 0 {
            let selected = (self.table.selected().unwrap_or(0) as isize + delta)
                .rem_euclid(length as isize) as usize;
            self.table.select(Some(selected));
            self.detail_tab = 0;
            self.detail_scroll = 0;
        }
    }

    fn move_tab(&mut self, delta: isize) {
        let count = self
            .selected()
            .and_then(|item| item.entry.snapshot().ok())
            .map_or(0, |snapshot| snapshot.tabs().count());
        if count > 0 {
            self.detail_tab =
                (self.detail_tab as isize + delta).rem_euclid(count as isize) as usize;
            self.detail_scroll = 0;
        }
    }

    fn handle_modal(&mut self, modal: Modal, key: KeyEvent) -> Effect {
        match modal {
            Modal::Confirm {
                title,
                lines,
                effect,
            } => match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => return effect,
                KeyCode::Char('n' | 'q') | KeyCode::Esc => self.set_notice("Cancelled.", false),
                _ => {
                    self.modal = Some(Modal::Confirm {
                        title,
                        lines,
                        effect,
                    })
                }
            },
            Modal::Input { prompt, mut value } => {
                match key.code {
                    KeyCode::Esc => return Effect::None,
                    KeyCode::Enter => {
                        let result = match prompt {
                            Prompt::Save => validate_name(&value)
                                .map(|()| Effect::Work(Job::Save(value.clone()))),
                            Prompt::Clean => self.prepare_cleanup(&value).map(|()| Effect::None),
                        };
                        match result {
                            Ok(effect) => return effect,
                            Err(error) => self.set_notice(error.to_string(), true),
                        }
                    }
                    KeyCode::Backspace => {
                        value.pop();
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        value.clear()
                    }
                    KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        value.push(character)
                    }
                    _ => {}
                }
                self.modal = Some(Modal::Input { prompt, value });
            }
        }
        Effect::None
    }

    fn confirm_restore(&mut self, close_existing: bool) {
        let Some(item) = self.selected() else {
            return;
        };
        let snapshot = match item.entry.snapshot() {
            Ok(snapshot) if !snapshot.windows.is_empty() => snapshot,
            Ok(_) => {
                self.set_notice("This snapshot has no windows to restore.", true);
                return;
            }
            Err(error) => {
                self.set_notice(error.to_string(), true);
                return;
            }
        };
        let selector = item
            .entry
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.modal = Some(Modal::Confirm {
            title: if close_existing {
                "Replace workspace"
            } else {
                "Restore into new windows"
            }
            .into(),
            lines: vec![
                snapshot.id.clone(),
                format!(
                    "{} windows · {} tabs · {} panes",
                    snapshot.windows.len(),
                    snapshot.tabs().count(),
                    snapshot.terminals().count()
                ),
                if close_existing {
                    "Existing Ghostty windows will close after restoration."
                } else {
                    "Existing Ghostty windows will stay open."
                }
                .into(),
            ],
            effect: Effect::Restore(RestoreOptions {
                selector: Some(selector),
                dry_run: false,
                close_existing,
            }),
        });
    }

    fn confirm_delete(&mut self) {
        let Some(item) = self.selected() else {
            return;
        };
        let size = match &item.usage {
            Ok(usage) => storage::format_bytes(usage.allocated),
            Err(error) => {
                self.set_notice(format!("Could not measure snapshot: {error}"), true);
                return;
            }
        };
        self.modal = Some(Modal::Confirm {
            title: "Delete snapshot".into(),
            lines: vec![
                item.entry.id().into(),
                format!("Frees {size} on disk."),
                "Deletes the snapshot and its scrollback. Agent conversation files are kept."
                    .into(),
            ],
            effect: Effect::Work(Job::Delete(vec![item.entry.path.clone()])),
        });
    }

    fn prepare_cleanup(&mut self, age: &str) -> Result<()> {
        let seconds = cli::parse_age(age)?;
        let duration = chrono::Duration::try_seconds(i64::try_from(seconds)?)
            .context("cleanup age is too large")?;
        let cutoff = Local::now()
            .checked_sub_signed(duration)
            .context("cleanup cutoff is out of range")?;
        let mut paths = Vec::new();
        let mut bytes = 0;
        let mut lines = Vec::new();
        // Preview the whole catalog, independent of the current search or sort.
        for item in &self.catalog.items {
            let Ok(snapshot) = item.entry.snapshot() else {
                continue;
            };
            if DateTime::parse_from_rfc3339(&snapshot.saved_at).is_ok_and(|saved| saved < cutoff) {
                let usage = item.usage.as_ref().map_err(|error| {
                    anyhow::anyhow!("could not measure {}: {error}", item.entry.id())
                })?;
                bytes += usage.allocated;
                paths.push(item.entry.path.clone());
                lines.push(item.entry.id().to_owned());
            }
        }
        if paths.is_empty() {
            self.set_notice(format!("No snapshots older than {age}."), false);
            return Ok(());
        }
        let count = paths.len();
        lines.truncate(5);
        if count > lines.len() {
            lines.push(format!("… and {} more", count - lines.len()));
        }
        lines.insert(
            0,
            format!(
                "{count} snapshots older than {age} · {} on disk",
                storage::format_bytes(bytes)
            ),
        );
        lines.push("Agent conversation files are kept. Invalid timestamps are skipped.".into());
        self.modal = Some(Modal::Confirm {
            title: "Confirm cleanup".into(),
            lines,
            effect: Effect::Work(Job::Delete(paths)),
        });
        Ok(())
    }
}

fn execute(app: &App, job: Job) -> Result<String> {
    match job {
        Job::Refresh => Ok("Snapshot list refreshed.".into()),
        Job::Save(name) => {
            app.save_snapshot(SaveOptions {
                name: name.clone(),
                capture_scrollback: true,
                current_window: false,
                window_id: None,
            })?;
            Ok(format!("Saved {name}."))
        }
        Job::Delete(paths) => {
            let count = paths.len();
            for (removed, path) in paths.iter().enumerate() {
                app.snapshots.remove(path).with_context(|| {
                    format!(
                        "deleted {removed} of {count} snapshots; could not remove {}",
                        path.display()
                    )
                })?;
            }
            Ok(format!("Removed {count} snapshot(s)."))
        }
    }
}

#[cfg(test)]
mod tests;
