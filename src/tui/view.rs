use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Clear, Gauge, Paragraph, Row, Table, Tabs, Wrap},
};

use super::{Manager, Modal, Page, Prompt};
use crate::{
    layout::{Direction, Layout as PaneLayout},
    model::{Snapshot, Tab},
    storage,
    ui::display_time,
};

const ACCENT: Color = Color::Rgb(108, 182, 255);
const GREEN: Color = Color::Rgb(105, 219, 154);
const AMBER: Color = Color::Rgb(255, 190, 92);
const MUTED: Color = Color::Rgb(130, 141, 158);

fn panel(title: impl Into<Line<'static>>) -> Block<'static> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
}

impl Manager {
    pub(super) fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        self.too_small = area.width < 64 || area.height < 20;
        if self.too_small {
            frame.render_widget(
                Paragraph::new(format!(
                    "rz v{}\nResize to at least 64 × 20.\nq quits.",
                    env!("CARGO_PKG_VERSION")
                ))
                .wrap(Wrap { trim: true }),
                area,
            );
            return;
        }
        let [header, stats, body, notice, help] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(3),
            Constraint::Length(2),
        ])
        .areas(area);
        let [brand, pages] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(header);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" rz ", Style::default().fg(ACCENT).bold()),
                Span::styled(
                    format!("v{}", env!("CARGO_PKG_VERSION")),
                    Style::default().fg(MUTED),
                ),
            ]))
            .block(Block::default().borders(Borders::BOTTOM)),
            brand,
        );
        frame.render_widget(
            Tabs::new(["Workspaces", "Storage"])
                .select(usize::from(self.page == Page::Storage))
                .highlight_style(Style::default().fg(ACCENT).bold())
                .divider(" │ ")
                .block(Block::default().borders(Borders::BOTTOM)),
            pages,
        );
        let agent_count: usize = self
            .catalog
            .items
            .iter()
            .filter_map(|item| item.entry.snapshot().ok())
            .map(|snapshot| snapshot.sessions().count())
            .sum();
        let stats_text = if self.searching || !self.query.is_empty() {
            format!(
                " / {}{}   ·   {}/{} snapshots",
                self.query,
                if self.searching { "▏" } else { "" },
                self.visible.len(),
                self.catalog.items.len()
            )
        } else {
            format!(
                " {} snapshots   ·   {} on disk   ·   {agent_count} saved agent sessions",
                self.catalog.items.len(),
                storage::format_bytes(self.catalog.usage.allocated)
            )
        };
        frame.render_widget(
            Paragraph::new(stats_text)
                .style(Style::default().fg(ACCENT))
                .block(panel(" Overview ")),
            stats,
        );
        let [list, details] = if body.width >= 108 {
            Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)]).areas(body)
        } else {
            Layout::vertical([Constraint::Percentage(46), Constraint::Percentage(54)]).areas(body)
        };
        self.draw_table(frame, list);
        self.draw_details(frame, details);
        let color = if self.error {
            Color::LightRed
        } else if self.busy {
            AMBER
        } else {
            ACCENT
        };
        frame.render_widget(
            Paragraph::new(self.notice.as_str())
                .wrap(Wrap { trim: true })
                .block(
                    panel(if self.error { " Error " } else { " Status " })
                        .border_style(Style::default().fg(color)),
                )
                .style(Style::default().fg(color)),
            notice,
        );
        frame.render_widget(Paragraph::new("↑↓/jk select · ←→ tabs · / search · o sort · Enter restore · R replace\ns save · d delete · c clean · r refresh · Tab storage · PgUp/Dn details · q quit")
            .style(Style::default().fg(MUTED)).alignment(Alignment::Center), help);
        if let Some(modal) = &self.modal {
            draw_modal(
                frame,
                area,
                modal,
                self.error.then_some(self.notice.as_str()),
            );
        }
    }

    fn draw_table(&mut self, frame: &mut Frame, area: Rect) {
        let rows = self.visible.iter().map(|index| {
            let item = &self.catalog.items[*index];
            let size = item
                .usage
                .as_ref()
                .map(|usage| storage::format_bytes(usage.allocated))
                .unwrap_or_else(|_| "?".into());
            match item.entry.snapshot() {
                Ok(snapshot) => Row::new(vec![
                    Cell::from(if snapshot.name.is_empty() {
                        snapshot.id.clone()
                    } else {
                        snapshot.name.clone()
                    }),
                    Cell::from(
                        chrono::DateTime::parse_from_rfc3339(&snapshot.saved_at)
                            .map(|time| time.format("%m-%d %H:%M").to_string())
                            .unwrap_or_else(|_| "?".into()),
                    ),
                    Cell::from(format!(
                        "{}/{}/{}",
                        snapshot.windows.len(),
                        snapshot.tabs().count(),
                        snapshot.terminals().count()
                    )),
                    Cell::from(size),
                ]),
                Err(_) => Row::new(vec![
                    item.entry.id().to_owned(),
                    "unreadable".into(),
                    "?".into(),
                    size,
                ])
                .style(Style::default().fg(Color::LightRed)),
            }
        });
        let table = Table::new(
            rows,
            [
                Constraint::Min(12),
                Constraint::Length(11),
                Constraint::Length(7),
                Constraint::Length(10),
            ],
        )
        .header(
            Row::new(["NAME", "SAVED", "W/T/P", "ON DISK"])
                .style(Style::default().fg(MUTED).bold())
                .bottom_margin(1),
        )
        .block(panel(format!(" Snapshots · {} ", self.sort.label())))
        .row_highlight_style(
            Style::default()
                .bg(Color::Rgb(34, 48, 65))
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(" › ");
        frame.render_stateful_widget(table, area, &mut self.table);
        if self.visible.is_empty() {
            frame.render_widget(
                Paragraph::new(if self.catalog.items.is_empty() {
                    "No snapshots yet. Press s to save a workspace."
                } else {
                    "No matches. Esc clears the search."
                })
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(MUTED)),
                Rect {
                    y: area.y.saturating_add(3),
                    height: area.height.saturating_sub(4),
                    x: area.x.saturating_add(2),
                    width: area.width.saturating_sub(4),
                },
            );
        }
    }

    fn draw_details(&self, frame: &mut Frame, area: Rect) {
        let Some(item) = self.selected() else {
            frame.render_widget(
                Paragraph::new(self.catalog.root.to_string_lossy())
                    .wrap(Wrap { trim: true })
                    .block(panel(" Storage location ")),
                area,
            );
            return;
        };
        let snapshot = match item.entry.snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                frame.render_widget(
                    Paragraph::new(format!("{error:#}\n\n{}", item.entry.path.display()))
                        .style(Style::default().fg(Color::LightRed))
                        .wrap(Wrap { trim: true })
                        .block(panel(" Unreadable snapshot ")),
                    area,
                );
                return;
            }
        };
        if self.page == Page::Storage {
            let [text, gauge] =
                Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).areas(area);
            let mut lines = vec![
                Line::from(snapshot.id.clone()).bold(),
                Line::from(format!(
                    "Saved {} · rz {}",
                    display_time(&snapshot.saved_at),
                    if snapshot.rz_version.is_empty() {
                        "legacy"
                    } else {
                        &snapshot.rz_version
                    }
                )),
                Line::from(format!(
                    "Snapshot on disk: {}",
                    item.usage
                        .as_ref()
                        .map(|usage| storage::format_bytes(usage.allocated))
                        .unwrap_or_else(|error| error.clone())
                )),
            ];
            lines.extend(scrollback_lines(snapshot));
            let legacy: usize = self
                .catalog
                .items
                .iter()
                .filter_map(|item| item.entry.snapshot().ok())
                .map(|snapshot| {
                    snapshot
                        .terminals()
                        .filter(|terminal| {
                            terminal
                                .scrollback_file
                                .as_ref()
                                .is_some_and(|file| !file.ends_with(".lz4"))
                        })
                        .count()
                })
                .sum();
            lines.push(Line::from(format!(
                "Storage total: {} · {legacy} legacy plain-text scrollback files",
                storage::format_bytes(self.catalog.usage.allocated)
            )));
            lines.push(Line::from(
                "c previews age-based cleanup across all snapshots.",
            ));
            lines.push(Line::from("d previews deletion of the selected snapshot."));
            lines.push(Line::from(
                "Agent conversation files live in their agents' own storage.",
            ));
            lines.push(Line::from(item.entry.path.display().to_string()).fg(MUTED));
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: true })
                    .scroll((self.detail_scroll, 0))
                    .block(panel(" Storage details ")),
                text,
            );
            let fraction = item
                .usage
                .as_ref()
                .map_or(0.0, |usage| {
                    usage.allocated as f64 / self.catalog.usage.allocated.max(1) as f64
                })
                .clamp(0.0, 1.0);
            frame.render_widget(
                Gauge::default()
                    .block(panel(" Share of snapshot storage "))
                    .gauge_style(Style::default().fg(ACCENT))
                    .ratio(fraction)
                    .label(format!("{:.1}%", fraction * 100.0)),
                gauge,
            );
            return;
        }
        let tab = snapshot.tabs().nth(self.detail_tab);
        let mut lines = vec![
            Line::from(snapshot.id.clone()).bold(),
            Line::from(format!(
                "{} · Ghostty {}",
                display_time(&snapshot.saved_at),
                snapshot.ghostty_version
            )),
            Line::from(
                snapshot
                    .agent_counts()
                    .map(|(agent, count)| format!("{} {count}", agent.label()))
                    .collect::<Vec<_>>()
                    .join(" · "),
            )
            .fg(ACCENT),
        ];
        if let Some(tab) = tab {
            lines.push(
                Line::from(format!(
                    "Tab {}/{} · {} · {}",
                    self.detail_tab + 1,
                    snapshot.tabs().count(),
                    tab.name,
                    tab.layout
                        .as_ref()
                        .map(|layout| layout.description())
                        .unwrap_or("geometry unavailable")
                ))
                .fg(GREEN),
            );
            for terminal in &tab.terminals {
                lines.push(Line::from(format!(
                    "Pane {} · {}",
                    terminal.index, terminal.working_directory
                )));
                if let Some(session) = terminal.session() {
                    lines.push(
                        Line::from(format!("  {} {}", session.agent.label(), session.id)).fg(MUTED),
                    );
                }
            }
        }
        lines.extend(
            snapshot
                .warnings
                .iter()
                .map(|warning| Line::from(format!("Warning: {warning}")).fg(AMBER)),
        );
        lines.extend(
            snapshot
                .limitations
                .iter()
                .map(|note| Line::from(note.as_str()).fg(MUTED)),
        );
        let (text, preview) = if area.height >= 12 {
            let [text, preview] =
                Layout::vertical([Constraint::Percentage(52), Constraint::Percentage(48)])
                    .areas(area);
            (text, Some(preview))
        } else {
            (area, None)
        };
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .scroll((self.detail_scroll, 0))
                .block(panel(" Selected · ←/→ browse tabs ")),
            text,
        );
        if let (Some(preview), Some(tab)) = (preview, tab) {
            let block = panel(" Layout preview ");
            let inner = block.inner(preview);
            frame.render_widget(block, preview);
            if let Some(layout) = &tab.layout {
                draw_layout(frame, inner, layout, tab);
            } else {
                frame.render_widget(
                    Paragraph::new("Split geometry was not captured.")
                        .wrap(Wrap { trim: true })
                        .fg(MUTED),
                    inner,
                );
            }
        }
    }
}

fn scrollback_lines(snapshot: &Snapshot) -> Vec<Line<'static>> {
    let files = snapshot
        .terminals()
        .filter(|terminal| terminal.scrollback_file.is_some())
        .count();
    let mut lines = vec![Line::from(format!(
        "Scrollback: {files} files · {}",
        if snapshot.scrollback_captured {
            "captured"
        } else {
            "skipped"
        }
    ))];
    if let (Some(original), Some(stored)) =
        (snapshot.scrollback_bytes, snapshot.scrollback_stored_bytes)
    {
        lines.push(Line::from(format!(
            "LZ4: {} → {}",
            storage::format_bytes(original),
            storage::format_bytes(stored)
        )));
        if original > 0 {
            lines.push(
                Line::from(format!(
                    "Scrollback savings: {:.1}%",
                    100.0 * (1.0 - stored as f64 / original as f64)
                ))
                .fg(GREEN),
            );
        }
    }
    lines
}

fn draw_layout(frame: &mut Frame, area: Rect, layout: &PaneLayout, tab: &Tab) {
    match layout {
        PaneLayout::Leaf { terminal } => {
            let pane = tab.terminals.iter().find(|pane| pane.index == *terminal);
            let title = pane.and_then(|pane| pane.session()).map_or_else(
                || format!(" {terminal} · shell "),
                |session| format!(" {terminal} · {} ", session.agent.label()),
            );
            let text = pane.map_or("", |pane| pane.working_directory.as_str());
            let color = if pane.is_some_and(|pane| pane.focused) {
                ACCENT
            } else {
                MUTED
            };
            frame.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: true })
                    .block(panel(title).border_style(Style::default().fg(color))),
                area,
            );
        }
        PaneLayout::Split {
            direction,
            first_size,
            total_size,
            first,
            second,
        } => {
            let total = (*total_size).clamp(1, i64::from(u32::MAX)) as u32;
            let first_size = (*first_size).clamp(0, i64::from(total)) as u32;
            let constraints = [
                Constraint::Ratio(first_size, total),
                Constraint::Ratio(total - first_size, total),
            ];
            let [a, b] = match direction {
                Direction::Horizontal => Layout::horizontal(constraints),
                Direction::Vertical => Layout::vertical(constraints),
            }
            .areas(area);
            draw_layout(frame, a, first, tab);
            draw_layout(frame, b, second, tab);
        }
    }
}

fn draw_modal(frame: &mut Frame, area: Rect, modal: &Modal, error: Option<&str>) {
    let (title, mut lines) = match modal {
        Modal::Input { prompt, value } => (
            match prompt {
                Prompt::Save => " Save workspace ",
                Prompt::Clean => " Cleanup age ",
            }
            .to_owned(),
            vec![
                Line::from(match prompt {
                    Prompt::Save => "Name, such as work or backup",
                    Prompt::Clean => "Remove snapshots older than this age, such as 7d or 2.weeks",
                }),
                Line::from(""),
                Line::from(format!(" > {value}▏")).fg(ACCENT),
                Line::from(""),
                Line::from("Enter continue · Esc cancel · Ctrl+U clear").fg(MUTED),
            ],
        ),
        Modal::Confirm { title, lines, .. } => {
            let mut lines = lines
                .iter()
                .map(|line| Line::from(line.as_str()))
                .collect::<Vec<_>>();
            lines.push(Line::from(""));
            lines.push(Line::from("y / Enter confirm · n / Esc cancel").fg(AMBER));
            (format!(" {title} "), lines)
        }
    };
    let footer = lines.pop().unwrap_or_default();
    if let (Modal::Input { .. }, Some(error)) = (modal, error) {
        lines.push(Line::from(error).fg(Color::LightRed));
    }
    let width = area.width.min(76);
    let text_width = usize::from(width.saturating_sub(2)).max(1);
    let height = lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(text_width))
        .sum::<usize>()
        .saturating_add(5)
        .min(usize::from(area.height)) as u16;
    let [vertical] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [popup] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(vertical);
    frame.render_widget(Clear, popup);
    let block = panel(title)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(AMBER));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let [body, help] = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(inner);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .alignment(Alignment::Left),
        body,
    );
    frame.render_widget(Paragraph::new(footer).wrap(Wrap { trim: true }), help);
}
