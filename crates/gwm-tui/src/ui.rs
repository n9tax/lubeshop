//! Rendering. Pure view code: it reads `App` state and draws — no logic.
//!
//! Colours come from the active [`Theme`], stashed in a thread-local for the
//! duration of a frame so the many small render helpers don't each need it
//! threaded through their signatures.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use gwm_core::library::{human_size, Integrity};
use gwm_core::models::MediaItem;

use crate::app::{App, Focus, LibRow, Screen, DRIVE_OPTIONS, MENU_ITEMS, TUNE_PARAMS};
use crate::count_job::CountState;
use crate::version_job::VersionState;
use crate::text_input::TextInput;
use crate::theme::{self, Theme};

/// Success check-mark. The default Windows console font frequently lacks U+2713
/// and renders it as a tofu box (a block with a `?`), so use an ASCII mark there;
/// keep the nicer glyph everywhere else. Kept to one column so the `[x]`/`[ ]`
/// tool badges stay aligned.
#[cfg(not(windows))]
const CHECK: &str = "✓";
#[cfg(windows)]
const CHECK: &str = "x";

thread_local! {
    static THEME: Cell<Theme> = const { Cell::new(theme::DARK) };
}

fn theme() -> Theme {
    THEME.with(Cell::get)
}

/// Base fill (theme text on theme background).
fn base() -> Style {
    Style::default().fg(theme().text).bg(theme().bg)
}

/// Highlight style for selected rows.
fn hl() -> Style {
    Style::default()
        .fg(theme().hl_fg)
        .bg(theme().hl_bg)
        .add_modifier(Modifier::BOLD)
}

/// Dimmed/secondary text.
fn dim() -> Style {
    Style::default().fg(theme().dim)
}

fn accented() -> Style {
    Style::default().fg(theme().accent).add_modifier(Modifier::BOLD)
}

pub fn render(app: &mut App, frame: &mut Frame) {
    THEME.with(|t| t.set(app.theme));
    let area = frame.area();
    // Paint the whole surface in the theme background first.
    frame.render_widget(Block::default().style(base()), area);

    // The bottom bar is one line for a key hint, but a notice can be long (e.g.
    // an install link), so grow it enough to wrap the whole message instead of
    // running off the edge.
    let status_h = match &app.notice {
        Some(notice) => {
            let w = area.width.saturating_sub(2).max(1) as usize;
            (((notice.chars().count() + 2) / w) as u16 + 1).clamp(1, 5)
        }
        None => 1,
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(status_h),
        ])
        .split(area);

    render_header(app, frame, chunks[0]);
    match app.screen {
        Screen::Menu => render_menu(app, frame, chunks[1]),
        Screen::Library => render_library(app, frame, chunks[1]),
        Screen::LibraryConfirmDelete => render_delete_confirm(app, frame, chunks[1]),
        Screen::LibraryRename => render_rename(app, frame, chunks[1]),
        Screen::EditNotes => render_edit_notes(app, frame, chunks[1]),
        Screen::NewFolder => render_new_folder(app, frame, chunks[1]),
        Screen::FormatPicker => render_format_picker(app, frame, chunks[1]),
        Screen::DrivePicker => render_drive_picker(app, frame, chunks[1]),
        Screen::NameInput => render_name_input(app, frame, chunks[1]),
        Screen::ReadOptions => render_read_options(app, frame, chunks[1]),
        Screen::Reading | Screen::ReadDone => render_reading(app, frame, chunks[1]),
        Screen::WriteSource => render_write_source(app, frame, chunks[1]),
        Screen::WriteConfirm => render_write_confirm(app, frame, chunks[1]),
        Screen::Writing | Screen::WriteDone => render_writing(app, frame, chunks[1]),
        Screen::Settings => render_settings(app, frame, chunks[1]),
        Screen::DriveTuning => render_drive_tuning(app, frame, chunks[1]),
        Screen::DriverPicker => render_driver_picker(app, frame, chunks[1]),
        Screen::OptionPicker => render_option_picker(app, frame, chunks[1]),
        Screen::Browse => render_browse(app, frame, chunks[1]),
        Screen::BrowseInput => render_browse_input(app, frame, chunks[1]),
        Screen::BrowseConfirmDelete => render_browse_confirm_delete(app, frame, chunks[1]),
        Screen::FileBrowse => render_file_browse(app, frame, chunks[1]),
        Screen::HexView => render_hex(app, frame, chunks[1]),
        Screen::TextEdit => render_text(app, frame, chunks[1]),
        Screen::NewImageName => render_new_image(app, frame, chunks[1]),
        Screen::Tools => render_tools(app, frame, chunks[1]),
        Screen::Installing => render_installing(app, frame, chunks[1]),
        Screen::ArchiveSearch => render_archive_search(app, frame, chunks[1]),
        Screen::ArchiveFetching => render_archive_fetching(app, frame, chunks[1]),
        Screen::ArchiveResults => render_archive_results(app, frame, chunks[1]),
        Screen::ArchiveFiles => render_archive_files(app, frame, chunks[1]),
        Screen::ArchiveDownloading => render_archive_downloading(app, frame, chunks[1]),
    }
    render_status(app, frame, chunks[2]);
}

fn bordered(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme().border).bg(theme().bg))
        .style(base())
        .title(format!(" {title} "))
        .title_style(accented())
}

fn para(lines: Vec<Line<'static>>) -> Paragraph<'static> {
    Paragraph::new(lines).style(base())
}

/// The `gw` status badge — green with the version when usable, red otherwise.
fn gw_badge(app: &App) -> Span<'static> {
    if app.core.gw.available {
        Span::styled(
            format!(" gw: {} ", app.core.gw.version.as_deref().unwrap_or("ok")),
            Style::default().fg(Color::Black).bg(theme().success),
        )
    } else {
        Span::styled(
            " gw: unavailable ",
            Style::default().fg(Color::White).bg(theme().danger),
        )
    }
}

fn render_header(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme().border).bg(theme().bg))
        .style(base());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Title centred; the `gw` status badge lives at the right of the same line,
    // freeing the whole footer for command hints.
    let title = Paragraph::new(Line::from(vec![
        Span::styled("The Lube Shop", accented()),
        Span::styled("  ·  Greaseweazle disk imaging", dim()),
    ]))
    .style(base())
    .alignment(Alignment::Center);
    frame.render_widget(title, inner);

    let badge = Paragraph::new(Line::from(gw_badge(app)))
        .style(base())
        .alignment(Alignment::Right);
    frame.render_widget(badge, inner);
}

fn render_menu(app: &App, frame: &mut Frame, area: Rect) {
    let items: Vec<ListItem> = MENU_ITEMS
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let selected = i == app.menu_index;
            let (marker, style) = if selected { ("▸ ", hl()) } else { ("  ", base()) };
            ListItem::new(Line::from(Span::styled(format!("{marker}{label}"), style)))
        })
        .collect();
    frame.render_widget(List::new(items).style(base()).block(bordered("Main Menu")), area);
}

fn render_library(app: &mut App, frame: &mut Frame, area: Rect) {
    if app.library.is_empty() {
        let empty = para(vec![
            Line::from(""),
            Line::from("Your library is empty."),
            Line::from(""),
            Line::from("Read a disk to populate it."),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(bordered("Library (0)"));
        frame.render_widget(empty, area);
        return;
    }

    let body = if app.lib_filtering || !app.lib_filter.is_empty() {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(area);
        let cursor = if app.lib_filtering { "▏" } else { "" };
        let filter = para(vec![Line::from(vec![
            Span::styled("Filter: ", dim()),
            Span::raw(app.lib_filter.clone()),
            Span::styled(cursor.to_string(), Style::default().fg(theme().accent)),
        ])])
        .block(bordered("Search  ('/' focus · Esc clear)"));
        frame.render_widget(filter, rows[0]);
        rows[1]
    } else {
        area
    };

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(body);

    let crumb = if app.lib_subpath.as_os_str().is_empty() {
        "Library".to_string()
    } else {
        format!("Library / {}", app.lib_subpath.display())
    };

    let rows = app.library_rows();
    let file_count = rows.iter().filter(|r| matches!(r, LibRow::File(_))).count();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match row {
            LibRow::Parent => ListItem::new(Line::from(Span::styled(
                "..".to_string(),
                Style::default().fg(theme().accent).add_modifier(Modifier::BOLD),
            ))),
            LibRow::Folder(name) => ListItem::new(Line::from(Span::styled(
                format!("{name}/"),
                Style::default().fg(theme().accent).add_modifier(Modifier::BOLD),
            ))),
            LibRow::File(id) => {
                let item = app.library.iter().find(|it| it.id == *id);
                let name = item
                    .map(|it| {
                        Path::new(&it.path)
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or(it.path.as_str())
                            .to_string()
                    })
                    .unwrap_or_default();
                let format = item.and_then(|it| it.format.as_deref()).unwrap_or("—");
                let kind = item.map(|it| it.kind.as_str()).unwrap_or("");
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{name:<24}"), Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(format!("{kind:<6}"), dim()),
                    Span::raw(format!(" {format}")),
                ]))
            }
        })
        .collect();

    let detail = app
        .lib_state
        .selected()
        .and_then(|i| rows.get(i))
        .and_then(|row| match row {
            LibRow::File(id) => app.library.iter().find(|it| it.id == *id).cloned(),
            _ => None,
        });

    let list = List::new(items)
        .style(base())
        .block(bordered(&format!("{crumb} ({file_count})")))
        .highlight_style(hl())
        .highlight_symbol("▸ ");
    frame.render_stateful_widget(list, cols[0], &mut app.lib_state);

    render_library_details(detail.as_ref(), &app.verify_results, frame, cols[1]);
}

fn detail_field(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:>8}: "), dim()),
        Span::styled(value.to_string(), Style::default().fg(theme().text)),
    ])
}

fn render_library_details(
    item: Option<&MediaItem>,
    verify: &HashMap<i64, Integrity>,
    frame: &mut Frame,
    area: Rect,
) {
    let block = bordered("Details");
    let Some(item) = item else {
        frame.render_widget(para(vec![Line::from("  No selection")]).block(block), area);
        return;
    };

    let name = Path::new(&item.path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(item.path.as_str());
    let sha = item
        .sha256
        .as_deref()
        .map(|s| format!("{}…", &s[..s.len().min(16)]))
        .unwrap_or_else(|| "—".to_string());

    let mut lines = vec![
        Line::from(Span::styled(name.to_string(), accented())),
        Line::from(""),
        detail_field("Kind", item.kind.as_str()),
        detail_field("Format", item.format.as_deref().unwrap_or("—")),
        detail_field("System", item.system.as_deref().unwrap_or("—")),
        detail_field("Size", &human_size(item.size_bytes)),
        detail_field("Source", item.source.as_str()),
        detail_field("Created", &item.created_at.format("%Y-%m-%d %H:%M").to_string()),
        detail_field("SHA-256", &sha),
    ];
    if !item.tags.is_empty() {
        lines.push(detail_field("Tags", &item.tags.join(", ")));
    }
    if let Some(notes) = &item.notes {
        lines.push(detail_field("Notes", notes));
    }
    if let Some(integrity) = verify.get(&item.id) {
        let color = match integrity {
            Integrity::Ok => theme().success,
            Integrity::Mismatch | Integrity::Missing => theme().danger,
            Integrity::NoBaseline => theme().warning,
        };
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Integrity: ", dim()),
            Span::styled(integrity.label().to_string(), Style::default().fg(color)),
        ]));
    }

    frame.render_widget(para(lines).block(block).wrap(Wrap { trim: false }), area);
}

fn render_delete_confirm(app: &App, frame: &mut Frame, area: Rect) {
    let name = app.selected_name();
    let danger = Style::default().fg(theme().danger).add_modifier(Modifier::BOLD);
    let toggle = if app.delete_file { "[x]" } else { "[ ]" };
    let consequence = if app.delete_file {
        Line::from(Span::styled("  ⚠  The image file will be permanently deleted.", danger))
    } else {
        Line::from(Span::styled(
            "  The file stays on disk; only the catalog entry is removed.",
            dim(),
        ))
    };
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  Remove "),
            Span::styled(format!("“{name}”"), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" from the library?"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(format!("  {toggle} "), Style::default().fg(theme().accent)),
            Span::raw("also delete the file on disk   (press 'f' to toggle)"),
        ]),
        Line::from(""),
        consequence,
        Line::from(""),
        Line::from(vec![
            Span::styled("  Press 'y' to confirm", danger),
            Span::styled("    ·    Esc / 'n' to cancel", dim()),
        ]),
    ];
    frame.render_widget(para(lines).block(bordered("Delete from library")), area);
}

/// Tab stop width for display (assembler source relies on this to align columns).
const TAB_WIDTH: usize = 8;

/// A single display glyph for a non-tab character: a visible marker for control/C1
/// bytes so they don't garble the terminal, otherwise the character itself. Tabs
/// are handled by [`expand_char`] (they become spaces to the next tab stop), not
/// here. Display-only — the editor buffer always keeps the real bytes.
fn display_char(c: char) -> char {
    let u = c as u32;
    if u < 0x20 || (0x7f..=0x9f).contains(&u) {
        '·'
    } else {
        c
    }
}

/// Append the on-screen form of `c` to `out`, advancing the display column `col`.
/// A tab expands to spaces up to the next multiple of [`TAB_WIDTH`] so columns line
/// up like a real editor; everything else is one cell.
fn expand_char(out: &mut String, col: &mut usize, c: char) {
    if c == '\t' {
        let stop = (*col / TAB_WIDTH + 1) * TAB_WIDTH;
        for _ in *col..stop {
            out.push(' ');
        }
        *col = stop;
    } else {
        out.push(display_char(c));
        *col += 1;
    }
}

fn render_text(app: &mut App, frame: &mut Frame, area: Rect) {
    let rows = area.height.saturating_sub(2) as usize;
    app.text_rows = rows.max(1);

    let total = app.text_lines.len();
    let title = format!(
        "{} — text · {} · ln {}/{} col {}{}  (Ctrl-S save · Esc leave)",
        app.text_title,
        app.text_eol.label(),
        app.text_row + 1,
        total,
        app.text_col + 1,
        if app.text_dirty { " *" } else { "" },
    );

    let base_style = Style::default().fg(theme().text);
    let cursor_style = Style::default()
        .fg(theme().hl_fg)
        .bg(theme().hl_bg)
        .add_modifier(Modifier::BOLD);

    let start = app.text_scroll.min(total.saturating_sub(1));
    let end = (start + rows).min(total);
    let mut lines: Vec<Line> = Vec::new();
    for r in start..end {
        let chars = &app.text_lines[r];
        if r == app.text_row {
            let col = app.text_col.min(chars.len());
            let mut dcol = 0usize;
            // Text left of the cursor, tab-expanded (tracking the display column).
            let mut before = String::new();
            for &c in &chars[..col] {
                expand_char(&mut before, &mut dcol, c);
            }
            let mut spans = vec![Span::styled(before, base_style)];
            if col < chars.len() {
                // Cursor on a character: highlight its expanded cell(s) — a tab
                // highlights the whole run of spaces to the next tab stop.
                let mut cell = String::new();
                expand_char(&mut cell, &mut dcol, chars[col]);
                spans.push(Span::styled(cell, cursor_style));
                let mut after = String::new();
                for &c in &chars[col + 1..] {
                    expand_char(&mut after, &mut dcol, c);
                }
                spans.push(Span::styled(after, base_style));
            } else {
                // Cursor past the last character: a trailing highlighted space.
                spans.push(Span::styled(" ", cursor_style));
            }
            lines.push(Line::from(spans));
        } else {
            let mut s = String::new();
            let mut dcol = 0usize;
            for &c in chars {
                expand_char(&mut s, &mut dcol, c);
            }
            lines.push(Line::from(Span::styled(s, base_style)));
        }
    }
    frame.render_widget(para(lines).block(bordered(&title)), area);
}

fn render_hex(app: &mut App, frame: &mut Frame, area: Rect) {
    let rows = area.height.saturating_sub(2) as usize;
    // Record the viewport height so the key handler can keep the cursor on-screen.
    app.hex_rows = rows.max(1);

    let data = &app.hex_data;
    let editing = app.hex_edit;
    let title = if editing {
        format!(
            "{} — EDIT [{}]  byte {:#x}/{}{}  (Tab switch · Ctrl-S save · Esc leave)",
            app.hex_title,
            if app.hex_ascii { "ascii" } else { "hex" },
            app.hex_cursor,
            data.len(),
            if app.hex_dirty { " *" } else { "" },
        )
    } else {
        format!(
            "{} — {} bytes @ {:#010x}{}",
            app.hex_title,
            data.len(),
            app.hex_offset,
            if app.hex_editable() { "   (e edit)" } else { "" },
        )
    };

    let text_style = Style::default().fg(theme().text);
    let ascii_style = Style::default().fg(theme().accent);
    // The active column's cursor cell is a solid highlight; the other column's
    // matching cell gets an underline so both panes track the cursor.
    let cursor_style = Style::default()
        .fg(theme().hl_fg)
        .bg(theme().hl_bg)
        .add_modifier(Modifier::BOLD);
    let mirror_style = Style::default()
        .fg(theme().accent)
        .add_modifier(Modifier::UNDERLINED);
    let cur = app.hex_cursor;

    let mut lines: Vec<Line> = Vec::new();
    let mut off = app.hex_offset.min(data.len());
    for _ in 0..rows {
        if off >= data.len() {
            break;
        }
        let end = (off + 16).min(data.len());
        let mut spans: Vec<Span> = vec![Span::styled(format!("{off:08x}  "), dim())];
        for i in 0..16 {
            if i == 8 {
                spans.push(Span::raw(" "));
            }
            let idx = off + i;
            match data.get(idx) {
                Some(b) => {
                    let style = if editing && idx == cur {
                        if app.hex_ascii { mirror_style } else { cursor_style }
                    } else {
                        text_style
                    };
                    spans.push(Span::styled(format!("{b:02x}"), style));
                    spans.push(Span::raw(" "));
                }
                None => spans.push(Span::raw("   ")),
            }
        }
        // ASCII pane, per-character so the cursor cell can be highlighted.
        spans.push(Span::styled(" │", ascii_style));
        for idx in off..end {
            let b = data[idx];
            let ch = if (0x20..0x7f).contains(&b) { b as char } else { '.' };
            let style = if editing && idx == cur {
                if app.hex_ascii { cursor_style } else { mirror_style }
            } else {
                ascii_style
            };
            spans.push(Span::styled(ch.to_string(), style));
        }
        spans.push(Span::styled("│", ascii_style));
        lines.push(Line::from(spans));
        off += 16;
    }
    if data.is_empty() {
        lines.push(Line::from(Span::styled("  (empty file)", dim())));
    }
    frame.render_widget(para(lines).block(bordered(&title)), area);
}

fn render_new_folder(app: &App, frame: &mut Frame, area: Rect) {
    let here = if app.lib_subpath.as_os_str().is_empty() {
        "library root".to_string()
    } else {
        app.lib_subpath.display().to_string()
    };
    let mut field = vec![Span::styled("  Folder name: ", dim())];
    field.extend(input_spans(&app.folder_input));
    let lines = vec![
        Line::from(""),
        Line::from(vec![Span::styled("  In: ", dim()), Span::raw(here)]),
        Line::from(""),
        Line::from(field),
        Line::from(""),
        Line::from(Span::styled(
            "  Creates a real folder on disk to organise media.",
            dim(),
        )),
        Line::from(Span::styled("  Enter to create · Esc to cancel", dim())),
    ];
    frame.render_widget(para(lines).block(bordered("New folder")), area);
}

fn render_edit_notes(app: &App, frame: &mut Frame, area: Rect) {
    let mut field = vec![Span::styled("  Notes: ", dim())];
    field.extend(input_spans(&app.notes_input));
    let lines = vec![
        Line::from(""),
        Line::from(field),
        Line::from(""),
        Line::from(Span::styled(
            "  Freeform notes stored with this image in the catalog.",
            dim(),
        )),
        Line::from(Span::styled(
            "  ←/→ move · Enter to save · Esc to cancel",
            dim(),
        )),
    ];
    frame.render_widget(para(lines).block(bordered("Edit notes")), area);
}

fn render_rename(app: &App, frame: &mut Frame, area: Rect) {
    let mut field = vec![Span::styled("  New name: ", dim())];
    field.extend(input_spans(&app.rename_input));
    let lines = vec![
        Line::from(""),
        Line::from(field),
        Line::from(""),
        Line::from(Span::styled(
            "  ←/→ move · Backspace/Del edit · Enter to rename · Esc to cancel",
            dim(),
        )),
    ];
    frame.render_widget(para(lines).block(bordered("Rename image")), area);
}

fn render_format_picker(app: &mut App, frame: &mut Frame, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    // Top bar: the label editor when active, otherwise the search filter.
    if let Some((fmt, input)) = app.format_editing.as_ref() {
        let mut spans = vec![Span::styled(format!("{fmt}  "), dim())];
        spans.extend(input_spans(input));
        frame.render_widget(
            para(vec![Line::from(spans)])
                .block(bordered("Edit label (Enter save · empty resets to default · Esc cancel)")),
            rows[0],
        );
    } else {
        frame.render_widget(
            para(vec![Line::from(vec![
                Span::raw(app.format_filter.clone()),
                Span::styled("▏", Style::default().fg(theme().accent)),
            ])])
            .block(bordered("Filter (type to search · ↑/↓ move · Enter pick · Ctrl+E edit label)")),
            rows[0],
        );
    }

    let matches = app.filtered_formats();
    let count = matches.len();
    let items: Vec<ListItem> = matches
        .iter()
        .map(|fmt| {
            let star = if app.is_recent_format(fmt) { "★ " } else { "  " };
            let custom = app.core.settings.format_labels.contains_key(*fmt);
            let label = app.format_label(fmt);
            let mut spans = vec![
                Span::styled(star, Style::default().fg(theme().accent)),
                Span::styled(format!("{fmt:<24}"), Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(label, dim()),
            ];
            if custom {
                spans.push(Span::styled("  ✎", Style::default().fg(theme().accent)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    drop(matches);

    let title = if app.is_decode_flow() {
        format!("Decode as… ({count} shown)")
    } else {
        format!("Formats ({count} shown)")
    };
    let list = List::new(items)
        .style(base())
        .block(bordered(&title))
        .highlight_style(hl())
        .highlight_symbol("▸ ");
    frame.render_stateful_widget(list, rows[1], &mut app.format_state);
}

fn render_drive_picker(app: &App, frame: &mut Frame, area: Rect) {
    let items: Vec<ListItem> = DRIVE_OPTIONS
        .iter()
        .enumerate()
        .map(|(i, (id, label))| {
            let selected = i == app.drive_index;
            let (marker, style) = if selected { ("▸ ", hl()) } else { ("  ", base()) };
            ListItem::new(Line::from(Span::styled(format!("{marker}{id}   {label}"), style)))
        })
        .collect();
    frame.render_widget(List::new(items).style(base()).block(bordered("Select Drive")), area);
}

/// Render a text field's contents with a visible block cursor at its position.
fn input_spans(input: &TextInput) -> Vec<Span<'static>> {
    let chars: Vec<char> = input.text().chars().collect();
    let cursor = input.cursor().min(chars.len());
    let before: String = chars[..cursor].iter().collect();
    let bold = Style::default().fg(theme().text).add_modifier(Modifier::BOLD);
    let cursor_style = Style::default().fg(theme().hl_fg).bg(theme().hl_bg);
    if cursor < chars.len() {
        let at = chars[cursor].to_string();
        let after: String = chars[cursor + 1..].iter().collect();
        vec![
            Span::styled(before, bold),
            Span::styled(at, cursor_style),
            Span::styled(after, bold),
        ]
    } else {
        vec![Span::styled(before, bold), Span::styled(" ", cursor_style)]
    }
}

fn render_name_input(app: &App, frame: &mut Frame, area: Rect) {
    let target = app.core.paths.library_dir.join(app.name_input.text().trim());
    let mut field = vec![Span::styled("  Filename: ", dim())];
    field.extend(input_spans(&app.name_input));
    let lines = vec![
        Line::from(""),
        Line::from(field),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Saves to: ", dim()),
            Span::raw(target.to_string_lossy().into_owned()),
        ]),
        Line::from(""),
        Line::from(Span::styled("  Enter to start reading · Esc to go back", dim())),
    ];
    frame.render_widget(para(lines).block(bordered("Name the image")), area);
}

fn render_read_options(app: &App, frame: &mut Frame, area: Rect) {
    let toggle = if app.read_hard_sectors { "[x]" } else { "[ ]" };
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Format: ", dim()),
            Span::styled(app.chosen_format.clone(), Style::default().add_modifier(Modifier::BOLD)),
            Span::styled("   Drive: ", dim()),
            Span::styled(app.chosen_drive.clone(), Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(format!("  {toggle} "), Style::default().fg(theme().accent)),
            Span::styled(
                "Hard-sectored disk",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled("   (Space/h to toggle)", dim()),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Turn on for NorthStar, Micropolis and other disks with physical",
            dim(),
        )),
        Line::from(Span::styled(
            "  sector holes (passes --hard-sectors to gw).",
            dim(),
        )),
        Line::from(""),
        Line::from(Span::styled("  Enter to continue · Esc to go back", dim())),
    ];
    frame.render_widget(para(lines).block(bordered("Read options")), area);
}

fn render_reading(app: &App, frame: &mut Frame, area: Rect) {
    let Some(job) = app.read_job.as_ref() else {
        return;
    };
    let done = app.screen == Screen::ReadDone;

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Min(0),
        ])
        .split(area);

    let info = para(vec![Line::from(vec![
        Span::styled("  Format ", dim()),
        Span::styled(job.format.clone(), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("   Drive ", dim()),
        Span::styled(job.drive.clone(), Style::default().add_modifier(Modifier::BOLD)),
    ])]);
    frame.render_widget(info, rows[0]);

    let ratio = job.progress_ratio();
    let label = match job.total_tracks {
        Some(total) => format!("{}/{} tracks — {}%", job.done_tracks, total, (ratio * 100.0) as u16),
        None => "starting…".to_string(),
    };
    frame.render_widget(gauge(ratio, label), rows[1]);

    frame.render_widget(para(vec![Line::from(format!("  {}", job.current))]), rows[2]);

    if done {
        render_outcome(app, job, frame, rows[3]);
    } else {
        let mut lines: Vec<Line> = Vec::new();
        if job.bad_tracks > 0 {
            lines.push(Line::from(Span::styled(
                format!("  weak tracks so far: {}", job.bad_tracks),
                Style::default().fg(theme().warning),
            )));
        }
        for note in job.notes.iter().rev().take(6).rev() {
            lines.push(Line::from(Span::styled(format!("  {note}"), dim())));
        }
        frame.render_widget(para(lines).block(bordered("Activity")), rows[3]);
    }
}

fn render_outcome(app: &App, job: &crate::read_job::ReadJob, frame: &mut Frame, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    match &app.read_outcome {
        Some(Ok(name)) => {
            lines.push(Line::from(Span::styled(
                format!("  {CHECK} Saved “{name}” to the library"),
                Style::default().fg(theme().success).add_modifier(Modifier::BOLD),
            )));
            if let Some((found, total, pct)) = job.summary {
                lines.push(Line::from(format!("  {found}/{total} sectors recovered ({pct}%)")));
            }
            if job.bad_tracks > 0 {
                lines.push(Line::from(Span::styled(
                    format!("  {} track(s) had unrecoverable sectors", job.bad_tracks),
                    Style::default().fg(theme().warning),
                )));
            }
        }
        Some(Err(msg)) => {
            lines.push(Line::from(Span::styled(
                "  ✗ Read failed",
                Style::default().fg(theme().danger).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(format!("  {msg}")));
        }
        None => {}
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("  Enter to return to the menu", dim())));
    frame.render_widget(para(lines).block(bordered("Done")), area);
}

fn render_write_source(app: &mut App, frame: &mut Frame, area: Rect) {
    let block = bordered(&format!("Select image to write ({})", app.library.len()));
    let items: Vec<ListItem> = app
        .library
        .iter()
        .map(|item| {
            let name = Path::new(&item.path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(item.path.as_str());
            let format = item.format.as_deref().unwrap_or("—");
            ListItem::new(Line::from(vec![
                Span::styled(format!("{name:<30}"), Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {format}"), dim()),
            ]))
        })
        .collect();
    let list = List::new(items)
        .style(base())
        .block(block)
        .highlight_style(hl())
        .highlight_symbol("▸ ");
    frame.render_stateful_widget(list, area, &mut app.write_state);
}

fn render_write_confirm(app: &App, frame: &mut Frame, area: Rect) {
    let danger = Style::default().fg(theme().danger).add_modifier(Modifier::BOLD);
    let erase = if app.write_erase { "[x]" } else { "[ ]" };
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  ⚠  This OVERWRITES the physical disk in the selected drive.",
            danger,
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Source: ", dim()),
            Span::styled(app.chosen_source_name.clone(), Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![Span::styled("  Format: ", dim()), Span::raw(app.chosen_format.clone())]),
        Line::from(vec![Span::styled("  Drive:  ", dim()), Span::raw(app.chosen_drive.clone())]),
        Line::from(vec![
            Span::styled(format!("  {erase} "), Style::default().fg(theme().accent)),
            Span::raw("pre-erase each track   (press 'e' to toggle)"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Press 'y' to WRITE", danger),
            Span::styled("    ·    Esc / 'n' to cancel", dim()),
        ]),
    ];
    frame.render_widget(para(lines).block(bordered("Confirm write — destructive")), area);
}

fn render_writing(app: &App, frame: &mut Frame, area: Rect) {
    let Some(job) = app.write_job.as_ref() else {
        return;
    };
    let done = app.screen == Screen::WriteDone;

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Min(0),
        ])
        .split(area);

    let info = para(vec![Line::from(vec![
        Span::styled("  Writing ", dim()),
        Span::styled(job.source.clone(), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("   Format ", dim()),
        Span::styled(job.format.clone(), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("   Drive ", dim()),
        Span::styled(job.drive.clone(), Style::default().add_modifier(Modifier::BOLD)),
    ])]);
    frame.render_widget(info, rows[0]);

    let ratio = job.progress_ratio();
    let label = match job.total_tracks {
        Some(total) => format!("{}/{} tracks — {}%", job.done_tracks, total, (ratio * 100.0) as u16),
        None => "starting…".to_string(),
    };
    frame.render_widget(gauge(ratio, label), rows[1]);

    frame.render_widget(para(vec![Line::from(format!("  {}", job.current))]), rows[2]);

    if done {
        render_write_outcome(app, job, frame, rows[3]);
    } else {
        let mut lines: Vec<Line> = Vec::new();
        if job.retries > 0 {
            lines.push(Line::from(Span::styled(
                format!("  verify retries so far: {}", job.retries),
                Style::default().fg(theme().warning),
            )));
        }
        for warning in job.warnings.iter().rev().take(6).rev() {
            lines.push(Line::from(Span::styled(format!("  {warning}"), dim())));
        }
        frame.render_widget(para(lines).block(bordered("Activity")), rows[3]);
    }
}

fn render_write_outcome(app: &App, job: &crate::write_job::WriteJob, frame: &mut Frame, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    match &app.write_outcome {
        Some(Ok(name)) => {
            lines.push(Line::from(Span::styled(
                format!("  {CHECK} Wrote “{name}” to the disk"),
                Style::default().fg(theme().success).add_modifier(Modifier::BOLD),
            )));
            if job.all_verified {
                lines.push(Line::from(Span::styled(
                    "  All tracks verified",
                    Style::default().fg(theme().success),
                )));
            } else if let Some((verified, not_verified, reason)) = &job.verify {
                lines.push(Line::from(Span::styled(
                    format!("  {verified} verified, {not_verified} not verified ({reason})"),
                    Style::default().fg(theme().warning),
                )));
            }
        }
        Some(Err(msg)) => {
            lines.push(Line::from(Span::styled(
                "  ✗ Write failed",
                Style::default().fg(theme().danger).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(format!("  {msg}")));
        }
        None => {}
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("  Enter to return to the menu", dim())));
    frame.render_widget(para(lines).block(bordered("Done")), area);
}

fn render_settings(app: &App, frame: &mut Frame, area: Rect) {
    let storage = if app.core.paths.store_is_default() {
        format!("(default) {}", app.core.paths.store_dir.display())
    } else {
        app.core.paths.store_dir.display().to_string()
    };
    let drive = DRIVE_OPTIONS
        .iter()
        .find(|(id, _)| *id == app.core.settings.default_drive)
        .map(|(id, label)| format!("{id}   {label}"))
        .unwrap_or_else(|| app.core.settings.default_drive.clone());

    let tuning = if app.core.settings.tuning.is_empty() {
        "default".to_string()
    } else {
        format!("{} override(s) — Enter", app.core.settings.tuning.len())
    };
    let rows = [
        ("Theme", format!("{}  —  {}", app.theme.name, app.theme.desc)),
        ("Store folder (all data)", storage),
        ("Default drive", drive),
        ("Drive tuning (gw delays)", tuning),
        (
            "Clean drive (zig-zag)",
            format!("drive {} — Enter to run", app.core.settings.default_drive),
        ),
    ];

    let mut lines = vec![Line::from("")];
    for (i, (label, value)) in rows.iter().enumerate() {
        let selected = i == app.settings_index;
        let marker = if selected { "▸ " } else { "  " };
        let label_style = if selected { accented() } else { base() };
        if i == 1 && app.settings_editing {
            let mut spans = vec![Span::styled(format!("{marker}{label}: "), label_style)];
            spans.extend(input_spans(&app.storage_input));
            lines.push(Line::from(spans));
        } else {
            lines.push(Line::from(vec![
                Span::styled(format!("{marker}{label}: "), label_style),
                Span::styled(value.clone(), Style::default().add_modifier(Modifier::BOLD)),
            ]));
        }
        lines.push(Line::from(""));
    }

    lines.push(Line::from(vec![
        Span::styled("  Preview: ", dim()),
        Span::styled(" accent ", Style::default().fg(theme().hl_fg).bg(theme().accent)),
        Span::raw(" "),
        Span::styled(" ok ", Style::default().fg(Color::Black).bg(theme().success)),
        Span::raw(" "),
        Span::styled(" warn ", Style::default().fg(Color::Black).bg(theme().warning)),
        Span::raw(" "),
        Span::styled(" danger ", Style::default().fg(Color::White).bg(theme().danger)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  ↑/↓ row · ←/→ change · Enter edit/cycle · Esc back",
        dim(),
    )));

    frame.render_widget(para(lines).block(bordered("Settings")), area);
}

fn render_drive_tuning(app: &App, frame: &mut Frame, area: Rect) {
    let mut lines = vec![Line::from("")];
    for (i, param) in TUNE_PARAMS.iter().enumerate() {
        let selected = i == app.tune_index;
        let marker = if selected { "▸ " } else { "  " };
        let value = app.tune_values.get(i).copied().unwrap_or(0);
        let overridden = app.core.settings.tuning.contains_key(param.name);
        let label_style = if selected { accented() } else { base() };
        let val_style = if overridden {
            Style::default().fg(theme().accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker}{:<16}", param.label), label_style),
            Span::styled(format!("{value} {}", param.unit), val_style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Slow drive (e.g. Shugart SA400)? Raise Step delay and Settle time until",
        dim(),
    )));
    lines.push(Line::from(Span::styled(
        "  it reads inner tracks. Applied live and re-applied before every read.",
        dim(),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  ↑/↓ select · ←/→ adjust · r reset all to defaults · Esc back",
        dim(),
    )));
    frame.render_widget(para(lines).block(bordered("Drive tuning — gw delays")), area);
}

fn render_driver_picker(app: &App, frame: &mut Frame, area: Rect) {
    let mut lines = vec![Line::from("")];
    for (i, kind) in app.driver_items.iter().enumerate() {
        let selected = i == app.driver_index;
        let available = app.driver_available.get(i).copied().unwrap_or(true);
        let marker = if selected { "▸ " } else { "  " };
        let style = if !available {
            dim()
        } else if selected {
            hl()
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        let mut spans = vec![Span::styled(format!("{marker}{}", kind.label()), style)];
        if !available {
            spans.push(Span::styled("  — not installed (Enter to get it)", dim()));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  ↑/↓ move · Enter choose · Esc back",
        dim(),
    )));
    frame.render_widget(para(lines).block(bordered("Choose a filesystem")), area);
}

fn render_option_picker(app: &mut App, frame: &mut Frame, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    if let Some((key, _, input)) = app.option_editing.as_ref() {
        let id = key.split_once(':').map(|(_, i)| i).unwrap_or(key);
        let mut spans = vec![Span::styled(format!("{id}  "), dim())];
        spans.extend(input_spans(input));
        frame.render_widget(
            para(vec![Line::from(spans)])
                .block(bordered("Edit label (Enter save · empty resets to default · Esc cancel)")),
            rows[0],
        );
    } else {
        frame.render_widget(
            para(vec![Line::from(vec![
                Span::raw(app.format_filter.clone()),
                Span::styled("▏", Style::default().fg(theme().accent)),
            ])])
            .block(bordered("Type to search · Enter pick · Ctrl+E edit label · Esc back")),
            rows[0],
        );
    }

    let matches = app.filtered_options();
    let count = matches.len();
    let items: Vec<ListItem> = matches
        .iter()
        .map(|opt| {
            let star = if app.is_recent_fs_format(&opt.id) { "★ " } else { "  " };
            let custom = app.fs_option_is_custom(&opt.id);
            let mut spans = vec![
                Span::styled(star, Style::default().fg(theme().accent)),
                Span::styled(format!("{:<16}", opt.id), Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(app.fs_option_label(opt), dim()),
            ];
            if custom {
                spans.push(Span::styled("  ✎", Style::default().fg(theme().accent)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    drop(matches);

    let list = List::new(items)
        .style(base())
        .block(bordered(&format!("Options ({count} shown)")))
        .highlight_style(hl())
        .highlight_symbol("▸ ");
    frame.render_stateful_widget(list, rows[1], &mut app.format_state);
}

fn panel(title: &str, focused: bool) -> Block<'static> {
    let border = if focused { theme().accent } else { theme().border };
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border).bg(theme().bg))
        .style(base())
        .title(format!(" {title} "))
        .title_style(accented())
}

fn render_browse(app: &mut App, frame: &mut Frame, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    // Capacity line.
    let usage_line = match app.browse_usage {
        Some(u) => {
            let pct = if u.total() > 0 { u.used * 100 / u.total() } else { 0 };
            Line::from(vec![
                Span::styled("  Capacity: ", dim()),
                Span::styled(
                    format!("{} used", human_size(u.used as i64)),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" of {}  ", human_size(u.total() as i64)), dim()),
                Span::styled(
                    format!("({} free, {pct}% used)", human_size(u.free as i64)),
                    Style::default().fg(if pct >= 90 { theme().danger } else { theme().accent }),
                ),
            ])
        }
        None => Line::from(Span::styled("  Capacity: unknown", dim())),
    };
    frame.render_widget(para(vec![usage_line]), outer[0]);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(outer[1]);

    render_browse_files(app, frame, cols[0]);
    render_browse_clipboard(app, frame, cols[1]);
}

fn render_browse_files(app: &mut App, frame: &mut Frame, area: Rect) {
    let focused = app.browse_focus == Focus::Files;
    // For a decoded flux master, name the *master* (the real artifact), not the
    // scratch working image.
    let named = app.browse_master.as_deref().unwrap_or(&app.browse_image);
    let image = Path::new(named)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("image")
        .to_string();
    // Show the CP/M diskdef when the driver needs one; otherwise the driver name
    // (self-describing filesystems don't have a user-chosen format).
    let mut descriptor = if app.browse_driver.needs_format() {
        app.browse_format.clone()
    } else {
        app.browse_driver.short_label().to_string()
    };
    if app.browse_master.is_some() {
        descriptor.push_str(" · decoded flux");
    }
    let title = format!("{image} · {descriptor} · {} file(s)", app.browse_entries.len());

    if app.browse_entries.is_empty() {
        let hint = if app.browse_driver.needs_format() {
            "  If this looks wrong, the\n  CP/M format may be incorrect."
        } else {
            "  The disk may be empty, or in a\n  format this driver can't read."
        };
        let mut lines = vec![Line::from(""), Line::from("  No files found."), Line::from("")];
        lines.extend(hint.lines().map(Line::from));
        frame.render_widget(para(lines).block(panel(&title, focused)), area);
        return;
    }

    let items: Vec<ListItem> = app
        .browse_entries
        .iter()
        .map(|entry| {
            // Mark files that have a stashed pristine original (R restores them).
            let edited = app.browse_originals.contains(&entry.name);
            let mut spans = vec![
                Span::styled(
                    format!("{}:{:<14}", entry.user, entry.name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(human_size(entry.size as i64), dim()),
            ];
            if edited {
                spans.push(Span::styled("  ● edited", Style::default().fg(theme().warning)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .style(base())
        .block(panel(&title, focused))
        .highlight_style(if focused { hl() } else { Style::default().add_modifier(Modifier::BOLD) })
        .highlight_symbol(if focused { "▸ " } else { "  " });
    frame.render_stateful_widget(list, area, &mut app.browse_state);
}

fn render_browse_clipboard(app: &mut App, frame: &mut Frame, area: Rect) {
    let focused = app.browse_focus == Focus::Clip;
    let block = panel(&format!("Clipboard ({})", app.clipboard.len()), focused);

    if app.clipboard.is_empty() {
        let empty = para(vec![
            Line::from(""),
            Line::from(Span::styled("  Empty.", dim())),
            Line::from(""),
            Line::from(Span::styled("  Press 'c' on a file to", dim())),
            Line::from(Span::styled("  copy it here, then open", dim())),
            Line::from(Span::styled("  another image and Tab", dim())),
            Line::from(Span::styled("  here to paste it in.", dim())),
        ])
        .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let items: Vec<ListItem> = app
        .clipboard
        .iter()
        .map(|item| ListItem::new(Line::from(Span::styled(item.name.clone(), Style::default().fg(theme().text)))))
        .collect();

    let list = List::new(items)
        .style(base())
        .block(block)
        .highlight_style(if focused { hl() } else { Style::default().add_modifier(Modifier::BOLD) })
        .highlight_symbol(if focused { "▸ " } else { "  " });
    frame.render_stateful_widget(list, area, &mut app.clip_state);
}

fn render_browse_input(app: &App, frame: &mut Frame, area: Rect) {
    let mut lines = vec![Line::from("")];
    if let Some(entry) = app
        .browse_state
        .selected()
        .and_then(|i| app.browse_entries.get(i))
    {
        lines.push(Line::from(vec![
            Span::styled("  File: ", dim()),
            Span::styled(entry.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
        ]));
        lines.push(Line::from(""));
    }
    let mut field = vec![Span::styled("  Save to:  ", dim())];
    field.extend(input_spans(&app.path_input));
    lines.push(Line::from(field));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Destination path (a directory keeps the file's original name)",
        dim(),
    )));
    lines.push(Line::from(Span::styled("  Enter to confirm · Esc to cancel", dim())));

    frame.render_widget(para(lines).block(bordered("Extract file from image")), area);
}

fn render_new_image(app: &App, frame: &mut Frame, area: Rect) {
    let target = app.core.paths.library_dir.join(app.create_input.text().trim());
    let mut field = vec![Span::styled("  Filename: ", dim())];
    field.extend(input_spans(&app.create_input));
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Type:     ", dim()),
            Span::styled(app.create_driver.label().to_string(), Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("  Format:   ", dim()),
            Span::styled(app.create_option.clone(), Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
        Line::from(field),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Creates:  ", dim()),
            Span::raw(target.to_string_lossy().into_owned()),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Enter to create a blank image · Esc to change format",
            dim(),
        )),
    ];
    frame.render_widget(para(lines).block(bordered("New blank image")), area);
}

fn render_file_browse(app: &mut App, frame: &mut Frame, area: Rect) {
    let pick_dir = app.file_browse_is_dir_mode();
    let Some(fb) = app.file_browser.as_mut() else {
        return;
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    let filter = para(vec![Line::from(vec![
        Span::styled("Find: ", dim()),
        Span::raw(fb.filter.clone()),
        Span::styled("▏", Style::default().fg(theme().accent)),
    ])])
    .block(bordered(if pick_dir {
        "Pick a storage folder  (→ open · Enter use this folder · ← up · Esc cancel)"
    } else {
        "Pick a file to insert  (→/Enter open · ← up · Esc cancel)"
    }));
    frame.render_widget(filter, rows[0]);

    let dir_title = fb.dir.display().to_string();
    let items: Vec<ListItem> = {
        let matches = fb.filtered();
        matches
            .iter()
            .map(|entry| {
                if entry.is_dir {
                    ListItem::new(Line::from(Span::styled(
                        format!("{}/", entry.name),
                        Style::default().fg(theme().accent).add_modifier(Modifier::BOLD),
                    )))
                } else {
                    ListItem::new(Line::from(Span::styled(
                        entry.name.clone(),
                        Style::default().fg(theme().text),
                    )))
                }
            })
            .collect()
    };

    let list = List::new(items)
        .style(base())
        .block(bordered(&dir_title))
        .highlight_style(hl())
        .highlight_symbol("▸ ");
    frame.render_stateful_widget(list, rows[1], &mut fb.state);
}

fn render_browse_confirm_delete(app: &App, frame: &mut Frame, area: Rect) {
    let name = app
        .browse_state
        .selected()
        .and_then(|i| app.browse_entries.get(i))
        .map(|e| e.name.clone())
        .unwrap_or_default();
    let danger = Style::default().fg(theme().danger).add_modifier(Modifier::BOLD);
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  Delete "),
            Span::styled(format!("“{name}”"), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" from the image?"),
        ]),
        Line::from(""),
        Line::from(Span::styled("  This modifies the image file in place.", dim())),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Press 'y' to delete", danger),
            Span::styled("    ·    Esc / 'n' to cancel", dim()),
        ]),
    ];
    frame.render_widget(para(lines).block(bordered("Delete file from image")), area);
}

fn render_tools(app: &App, frame: &mut Frame, area: Rect) {
    let mut lines = vec![Line::from("")];
    for (i, tool) in gwm_core::tools::TOOLS.iter().enumerate() {
        let installed = app.tool_status.get(i).copied().unwrap_or(false);
        let selected = i == app.tools_index;
        let marker = if selected { "▸ " } else { "  " };
        let (badge, badge_style) = if installed {
            (format!("[{CHECK}]"), Style::default().fg(theme().success))
        } else {
            ("[ ]".to_string(), Style::default().fg(theme().danger))
        };
        let label_style = if selected {
            accented()
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        let mut spans = vec![
            Span::styled(marker.to_string(), Style::default().fg(theme().accent)),
            Span::styled(format!("{badge} "), badge_style),
            Span::styled(format!("{:<20}", tool.label), label_style),
            Span::styled(format!("{:<38}", tool.purpose), dim()),
        ];
        // Installed version + "update available" badge, filled in asynchronously.
        match app.tool_versions.get(i) {
            Some(VersionState::Ready(Some(v))) => {
                spans.push(Span::styled(format!("v{v}"), dim()));
                if let Some(target) = tool.version {
                    if gwm_core::tools::is_outdated(v, target) {
                        spans.push(Span::styled(
                            format!("  ⬆ update → v{target}"),
                            Style::default().fg(theme().warning),
                        ));
                    }
                }
            }
            Some(VersionState::Pending) if installed => {
                spans.push(Span::styled("…", dim()));
            }
            _ => {}
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Enter installs the selected tool with your system's package manager",
        dim(),
    )));
    lines.push(Line::from(Span::styled(
        "  (apt/dnf/zypper/AUR, or pipx) in the terminal — it may ask for your password.",
        dim(),
    )));
    lines.push(Line::from(Span::styled(
        "  Tools with no package for your distro are built from source for you.",
        dim(),
    )));
    frame.render_widget(para(lines).block(bordered("Tools")), area);
}

fn render_installing(app: &App, frame: &mut Frame, area: Rect) {
    let Some(job) = app.install_job.as_ref() else {
        return;
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    let status = if !job.finished {
        Span::styled(format!("  {}…", job.label), accented())
    } else if job.success {
        Span::styled(
            format!("  {CHECK} {} — done", job.label),
            Style::default().fg(theme().success).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            format!("  ✗ {} failed (see output)", job.label),
            Style::default().fg(theme().danger).add_modifier(Modifier::BOLD),
        )
    };
    frame.render_widget(para(vec![Line::from(status)]), rows[0]);

    // Show the tail of the output that fits.
    let height = rows[1].height.saturating_sub(2) as usize;
    let start = job.lines.len().saturating_sub(height.max(1));
    let mut lines: Vec<Line> = job.lines[start..]
        .iter()
        .map(|l| Line::from(Span::styled(l.clone(), dim())))
        .collect();
    if job.finished {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("  Enter to return to Tools", dim())));
    }
    frame.render_widget(para(lines).block(bordered("Output")), rows[1]);
}

fn gauge(ratio: f64, label: String) -> Gauge<'static> {
    Gauge::default()
        .block(bordered("Progress"))
        .style(base())
        .gauge_style(Style::default().fg(theme().accent).bg(theme().bg))
        .ratio(ratio)
        .label(label)
}

// --- archive.org import screens ------------------------------------------

fn render_archive_search(app: &App, frame: &mut Frame, area: Rect) {
    let mut field = vec![Span::styled("  Search: ", dim())];
    field.extend(input_spans(&app.archive_query));
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Search the Internet Archive for disk images to import.",
            dim(),
        )),
        Line::from(""),
        Line::from(field),
        Line::from(""),
        Line::from(Span::styled(
            "  Tip: try a title, or a qualifier like  subject:Amiga  ·  mediatype:software",
            dim(),
        )),
        Line::from(""),
        Line::from(Span::styled("  Enter to search · Esc to go back", dim())),
    ];
    frame.render_widget(para(lines).block(bordered("Import from archive.org")), area);
}

fn render_archive_fetching(app: &App, frame: &mut Frame, area: Rect) {
    let label = app
        .net_job_label()
        .unwrap_or_else(|| "Contacting archive.org…".to_string());
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(format!("  {label}"), accented())),
        Line::from(""),
        Line::from(Span::styled("  Esc to cancel and go back", dim())),
    ];
    frame.render_widget(para(lines).block(bordered("archive.org")), area);
}

fn render_archive_results(app: &mut App, frame: &mut Frame, area: Rect) {
    let items: Vec<ListItem> = app
        .archive_hits
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let title = if h.title.is_empty() { &h.identifier } else { &h.title };
            let title: String = title.chars().take(56).collect();
            let badge = match app.archive_counts.get(i) {
                Some(CountState::Ready(0)) => {
                    Span::styled("  no images".to_string(), Style::default().fg(theme().warning))
                }
                Some(CountState::Ready(n)) => Span::styled(
                    format!("  {n} image{}", if *n == 1 { "" } else { "s" }),
                    Style::default().fg(theme().success).add_modifier(Modifier::BOLD),
                ),
                Some(CountState::Failed) => Span::styled("  ?".to_string(), dim()),
                Some(CountState::Pending) | None => Span::styled("  …".to_string(), dim()),
            };
            ListItem::new(Line::from(vec![
                Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(format!("   [{}]  {} ↓", h.mediatype, h.downloads), dim()),
                badge,
            ]))
        })
        .collect();
    let title = format!("Results — {} item(s)", app.archive_hits.len());
    let list = List::new(items)
        .style(base())
        .block(bordered(&title))
        .highlight_style(hl())
        .highlight_symbol("▸ ");
    frame.render_stateful_widget(list, area, &mut app.archive_hits_state);
}

fn render_archive_files(app: &mut App, frame: &mut Frame, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(area);

    let header: String = app.archive_item_title.chars().take(72).collect();
    frame.render_widget(
        para(vec![Line::from(vec![
            Span::styled("  Item: ", dim()),
            Span::styled(header, Style::default().add_modifier(Modifier::BOLD)),
        ])]),
        rows[0],
    );

    let image_count = app.archive_files.iter().filter(|f| f.is_image()).count();
    let items: Vec<ListItem> = app
        .archive_files
        .iter()
        .map(|f| {
            let (tag, tag_style) = if f.is_image() {
                ("◆ disk image", Style::default().fg(theme().success))
            } else if f.is_container() {
                ("▤ archive — opened on download", Style::default().fg(theme().accent))
            } else {
                ("· other file", dim())
            };
            let gz = if f.is_gzipped() { "  (→ adf)" } else { "" };
            ListItem::new(Line::from(vec![
                Span::styled(f.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(format!("   {}", human_size(f.size as i64)), dim()),
                Span::styled(gz.to_string(), dim()),
                Span::styled(format!("   {tag}"), tag_style),
            ]))
        })
        .collect();
    let title = format!(
        "Files — {} shown, {} disk image(s)",
        app.archive_files.len(),
        image_count
    );
    let list = List::new(items)
        .style(base())
        .block(bordered(&title))
        .highlight_style(hl())
        .highlight_symbol("▸ ");
    frame.render_stateful_widget(list, rows[1], &mut app.archive_files_state);
}

fn render_archive_downloading(app: &App, frame: &mut Frame, area: Rect) {
    let Some(job) = app.download_job.as_ref() else {
        return;
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    frame.render_widget(
        para(vec![Line::from(vec![
            Span::styled("  Downloading ", dim()),
            Span::styled(job.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
        ])]),
        rows[0],
    );

    let ratio = job.progress_ratio();
    let label = if job.total > 0 {
        format!(
            "{} / {} — {}%",
            human_size(job.done as i64),
            human_size(job.total as i64),
            (ratio * 100.0) as u16
        )
    } else {
        format!("{} downloaded…", human_size(job.done as i64))
    };
    frame.render_widget(gauge(ratio, label), rows[1]);

    frame.render_widget(
        para(vec![Line::from(Span::styled(
            "  Verifying checksum and cataloguing on completion…",
            dim(),
        ))]),
        rows[2],
    );
}

fn render_status(app: &App, frame: &mut Frame, area: Rect) {
    let hint = if let Some(notice) = &app.notice {
        Span::styled(format!("  {notice}"), Style::default().fg(theme().warning))
    } else {
        let text = match app.screen {
            Screen::Menu => "  ↑/↓ move · Enter select · q quit",
            Screen::Library => "  ↑/↓ · Enter open · m mkdir · b browse · f format · h hex · n notes · r rename · d del",
            Screen::NewFolder => "  type name · Enter create · Esc cancel",
            Screen::TextEdit => {
                "  arrows move · type to edit · Enter/Backspace/Del · Ctrl-S save · Esc leave"
            }
            Screen::HexView => {
                if app.hex_edit {
                    "  arrows move · Tab hex/ascii · type to overtype · Ctrl-S save · Esc leave"
                } else if app.hex_editable() {
                    "  ↑/↓ line · PgUp/PgDn · Home/End · e edit · Esc back"
                } else {
                    "  ↑/↓ line · PgUp/PgDn · Home/End · Esc back"
                }
            }
            Screen::LibraryConfirmDelete => "  y confirm · f toggle file · Esc cancel",
            Screen::LibraryRename => "  type name · Enter rename · Esc cancel",
            Screen::EditNotes => "  type notes · Enter save · Esc cancel",
            Screen::FormatPicker => {
                "  type to filter · ↑/↓ · Enter pick · Ctrl+E edit label · Esc back"
            }
            Screen::DrivePicker => "  ↑/↓ move · Enter select · Esc back",
            Screen::NameInput => "  type a name · Enter start · Esc back",
            Screen::ReadOptions => "  Space/h toggle hard-sectors · Enter continue · Esc back",
            Screen::Reading => "  reading… please wait",
            Screen::ReadDone => "  Enter return to menu",
            Screen::WriteSource => "  ↑/↓ move · Enter select · Esc back",
            Screen::WriteConfirm => "  y write · e toggle erase · Esc cancel",
            Screen::Writing => "  writing… please wait",
            Screen::WriteDone => "  Enter return to menu",
            Screen::Settings => "  ↑/↓ row · ←/→ change · Enter edit/open · Esc back",
            Screen::DriveTuning => "  ↑/↓ select · ←/→ adjust · r reset defaults · Esc back",
            Screen::DriverPicker => "  ↑/↓ move · Enter choose · Esc back",
            Screen::OptionPicker => {
                "  type to filter · ↑/↓ · Enter pick · Ctrl+E edit label · Esc back"
            }
            Screen::Browse => match app.browse_focus {
                Focus::Files => "  Tab · x extract · i insert · c copy · t text · h hex · R restore · d delete · Esc",
                Focus::Clip => "  Tab panes · Enter/p paste into image · d remove · Esc",
            },
            Screen::BrowseInput => "  type a path · Enter confirm · Esc cancel",
            Screen::BrowseConfirmDelete => "  y delete · Esc cancel",
            Screen::FileBrowse => {
                if app.file_browse_is_dir_mode() {
                    "  ↑/↓ · → open folder · ← up · type to find · Enter use this folder · Esc"
                } else {
                    "  ↑/↓ · → open · ← up · type to find · Enter pick · Esc"
                }
            }
            Screen::NewImageName => "  type a name · Enter create · Esc back",
            Screen::Tools => "  ↑/↓ move · Enter install · Esc back",
            Screen::Installing => {
                if app.install_job.as_ref().map(|j| j.finished).unwrap_or(true) {
                    "  Enter to return"
                } else {
                    "  installing… please wait"
                }
            }
            Screen::ArchiveSearch => "  type a query · Enter search · Esc back",
            Screen::ArchiveFetching => "  contacting archive.org… · Esc cancel",
            Screen::ArchiveResults => {
                "  ↑/↓ move · Enter open item · Esc new search · counts show importable images"
            }
            Screen::ArchiveFiles => {
                "  ↑/↓ move · Enter download (images import · zips unpack · loose files → clipboard) · Esc back"
            }
            Screen::ArchiveDownloading => "  downloading… please wait",
        };
        Span::raw(text)
    };

    frame.render_widget(para(vec![Line::from(hint)]).wrap(Wrap { trim: false }), area);
}

#[cfg(test)]
mod render_smoke {
    use super::*;
    use crate::app::App;
    use gwm_core::Core;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::Terminal;

    #[test]
    fn file_browse_dir_mode_renders() {
        let mut app = App::new(Core::init().unwrap());
        // Drive the real path: Settings → storage row → Enter opens the dir picker.
        app.screen = Screen::Settings;
        app.settings_index = 1;
        app.test_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.screen, Screen::FileBrowse);
        assert!(app.file_browse_is_dir_mode());
        for name in ["dark", "borland"] {
            app.theme = crate::theme::by_name(name);
            let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
            t.draw(|f| render(&mut app, f)).unwrap();
        }
    }

    #[test]
    fn archive_screens_render() {
        use gwm_core::archive::{RemoteFile, SearchHit};
        let mut app = App::new(Core::init().unwrap());

        // Menu → "Import from archive.org" (index 4) → search screen.
        for _ in 0..4 {
            app.test_key(KeyCode::Down, KeyModifiers::NONE);
        }
        app.test_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.screen, Screen::ArchiveSearch);
        for c in "amiga".chars() {
            app.test_key(KeyCode::Char(c), KeyModifiers::NONE);
        }

        // Populate results + files as the workers would, and render each screen.
        app.archive_hits = vec![SearchHit {
            identifier: "some-item".into(),
            title: "Some Amiga Disks".into(),
            downloads: 1234,
            mediatype: "software".into(),
        }];
        app.archive_hits_state.select(Some(0));
        app.archive_files = vec![RemoteFile {
            name: "Workbench.adz".into(),
            size: 901_120,
            sha1: None,
            identifier: "some-item".into(),
        }];
        app.archive_files_state.select(Some(0));
        app.archive_item_title = "Some Amiga Disks".into();

        for screen in [
            Screen::ArchiveSearch,
            Screen::ArchiveFetching,
            Screen::ArchiveResults,
            Screen::ArchiveFiles,
            Screen::ArchiveDownloading,
        ] {
            app.screen = screen;
            let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
            t.draw(|f| render(&mut app, f)).unwrap();
        }
    }
}
