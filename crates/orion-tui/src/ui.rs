//! UI drawing.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Cell, Clear, List, ListItem, Paragraph, Row, Table, Wrap};
use ratatui::Frame;
use ratatui_image::picker::Capability;
use ratatui_image::sliced::{SignedPosition, SlicedImage};
use ratatui_image::{Resize, StatefulImage};

use orion_core::model::{human_size, CryptoProblem, CryptoReport, SigStatus, Trust};

use crate::app::{App, Focus, Mode};
use crate::body::{to_line, BodyMode, InlineImage};
use crate::keys::Action;
use crate::theme::{Icon, Theme};
use orion_core::config::{LayoutMode, ListStyle};

fn t() -> &'static Theme {
    crate::theme::theme()
}

fn pane(title: String, focused: bool) -> Block<'static> {
    let th = t();
    let (border, title_style) = if focused {
        (Style::new().fg(th.accent), Style::new().fg(th.accent).bold())
    } else {
        (Style::new().fg(th.border), Style::new().fg(th.subtext))
    };
    Block::bordered().border_type(BorderType::Rounded).border_style(border).title(Span::styled(title, title_style))
}

/// Background of the selected row.
fn selected(focused: bool) -> Style {
    Style::new().bg(if focused { t().selection_focused } else { t().selection })
}

/// `left` and `right` on one line, `right` flush with the edge.
fn spread(mut left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: u16) -> Line<'static> {
    let used: usize = left.iter().chain(&right).map(|s| s.width()).sum();
    left.push(Span::raw(" ".repeat((width as usize).saturating_sub(used))));
    left.extend(right);
    Line::from(left)
}

/// "14:32" today, "yesterday", "Mon" this week, "12 Nov" this year, else "12 Nov 2024".
pub fn short_date(ts: i64) -> String {
    use chrono::{Datelike, Local, TimeZone};
    if ts == 0 {
        return String::new();
    }
    let Some(t) = Local.timestamp_opt(ts, 0).single() else { return String::new() };
    let now = Local::now();
    let (day, today) = (t.date_naive(), now.date_naive());
    if day == today {
        t.format("%H:%M").to_string()
    } else if today.pred_opt() == Some(day) {
        "yesterday".into()
    } else if day < today && (today - day).num_days() < 7 {
        t.format("%a").to_string()
    } else if t.year() == now.year() {
        t.format("%-d %b").to_string()
    } else {
        t.format("%-d %b %Y").to_string()
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, status] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
    let [side, right] =
        Layout::horizontal([Constraint::Length(app.cfg.ui.sidebar_width), Constraint::Fill(1)]).areas(main);

    // Three panes when there is room for a readable list and message.
    app.wide = match app.cfg.ui.layout {
        LayoutMode::ThreePane => true,
        LayoutMode::Stacked => false,
        LayoutMode::Auto => right.width >= 120,
    };
    draw_sidebar(f, app, side);
    if app.compose.is_some() {
        draw_compose(f, app, right);
    } else if app.wide {
        let list_w = (right.width * 2 / 5).clamp(40, 80);
        let [list, message] = Layout::horizontal([Constraint::Length(list_w), Constraint::Fill(1)]).areas(right);
        draw_list(f, app, list);
        if app.reader.is_some() {
            draw_reader(f, app, message);
        } else {
            let hint = if app.visible.is_empty() { "" } else { "select a message" };
            f.render_widget(Paragraph::new(hint).style(Style::new().fg(t().overlay)).centered().block(pane(" Message ".into(), false)), message);
        }
    } else if app.reader.is_some() {
        draw_reader(f, app, right);
    } else {
        draw_list(f, app, right);
    }
    draw_status(f, app, status);
    if let Some(p) = app.compose.as_ref().and_then(|c| c.picker.as_ref()) {
        draw_file_picker(f, p);
    }
    if matches!(app.mode, Mode::Help) {
        draw_help(f, app);
    }
    if let Mode::Attachments { sel } = &app.mode {
        draw_attachments_popup(f, app, *sel);
    }
    if let Mode::MoveTo { key, filter, sel, .. } = &app.mode {
        draw_move_popup(f, &app.move_targets(key, filter), *sel);
    }
    if let Mode::Card(lines) = &app.mode {
        draw_popup(f, " OpenPGP card (any key to close) ", lines);
    }
}

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let th = t();
    let focused = app.focus == Focus::Sidebar;
    let block = pane(" Mailboxes ".into(), focused);
    let width = block.inner(area).width;
    let mut items = Vec::new();
    let mut selected_item = None;
    let mut flat = 0;
    for (n, acc) in app.accounts.iter().enumerate() {
        if n > 0 {
            items.push(ListItem::new(Line::raw("")));
        }
        let mut head = vec![
            Span::styled(format!(" {} ", Icon::Account.glyph()), Style::new().fg(th.accent2)),
            Span::styled(acc.name.clone(), Style::new().fg(th.accent2).bold()),
        ];
        if !acc.progress.is_empty() {
            head.push(Span::styled(format!(" {}", Icon::Syncing.glyph()), Style::new().fg(th.warn)));
        }
        items.push(ListItem::new(Line::from(head)));
        if acc.mailboxes.is_empty() && acc.error.is_none() {
            items.push(ListItem::new(Line::styled("    connecting…", Style::new().fg(th.overlay))));
        }
        if let Some(err) = &acc.error {
            items.push(ListItem::new(Line::styled(format!("  ⚠ {err}"), Style::new().fg(th.error))));
        }
        for mb in &acc.mailboxes {
            if flat == app.sidebar_sel {
                selected_item = Some(items.len());
            }
            let is_current = app.current.is_some() && app.sidebar.get(flat) == app.current.as_ref();
            // Folder tree: "Parent/Child" is shown as an indented "Child".
            let depth = mb.name.matches('/').count();
            let leaf = mb.name.rsplit('/').next().unwrap_or(&mb.name).to_string();
            let icon = Icon::for_mailbox(&mb.id, &mb.name);
            let icon_color = match icon {
                Icon::Inbox => th.info,
                Icon::Trash | Icon::Junk => th.overlay,
                _ => th.subtext,
            };
            let name_style = match (is_current, mb.unread > 0) {
                (true, _) => Style::new().fg(th.accent).bold(),
                (false, true) => Style::new().fg(th.text).bold(),
                (false, false) => Style::new().fg(th.subtext),
            };
            let left = vec![
                Span::styled(if is_current { " ▌" } else { "  " }, Style::new().fg(th.accent)),
                Span::raw("  ".repeat(depth + 1)),
                Span::styled(format!("{} ", icon.glyph()), Style::new().fg(if is_current { th.accent } else { icon_color })),
                Span::styled(leaf, name_style),
            ];
            let right = if mb.unread > 0 { vec![Span::styled(format!("{} ", mb.unread), Style::new().fg(th.unread).bold())] } else { vec![] };
            items.push(ListItem::new(spread(left, right, width)));
            flat += 1;
        }
    }
    app.sidebar_state.select(selected_item);
    let list = List::new(items).block(block).highlight_style(selected(focused));
    f.render_stateful_widget(list, area, &mut app.sidebar_state);
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let th = t();
    let syncing = if app.sync_error.is_some() { " · sync failed" } else if app.loading { " · syncing…" } else { "" };
    let account = app.current.as_ref().and_then(|k| app.accounts.get(k.0)).map(|a| a.name.clone()).unwrap_or_default();
    let title = match app.current_mailbox() {
        Some(mb) if !app.filter.is_empty() => {
            format!(" {account} · {} — {}/{} — filter: {}{syncing} ", mb.name, app.visible.len(), app.native, app.filter)
        }
        Some(mb) => format!(" {account} · {} · {}{syncing} ", mb.name, app.native),
        None => " No mailbox ".into(),
    };
    let focused = app.focus == Focus::List;
    let block = pane(title, focused);
    let inner = block.inner(area);
    let two_line = app.cfg.ui.list_style == ListStyle::TwoLine;
    // Rows per message: sender, then subject.
    let row_h: u16 = if two_line { 2 } else { 1 };
    app.list_page = (inner.height / row_h).max(1);

    // Only visible rows are built: lists of 100k messages stay instant.
    let height = app.list_page as usize;
    let sel = app.list_state.selected().unwrap_or(0);
    let offset = {
        let cur = app.list_state.offset();
        if sel < cur { sel } else if sel >= cur + height { sel + 1 - height } else { cur }
    };
    let date_w: u16 = 12;
    let rows: Vec<Row> = app
        .visible
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(pos, &i)| {
            let e = &app.envelopes[i];
            let info = app.rows.get(pos).cloned().unwrap_or_default();
            let thread = !info.members.is_empty();
            // A conversation counts as unread / flagged / with attachments if any message is.
            let members: &[usize] = if thread { &info.members } else { std::slice::from_ref(&i) };
            let any = |f: &dyn Fn(&orion_core::model::Envelope) -> bool| members.iter().any(|&m| f(&app.envelopes[m]));
            let unread = any(&|m| !m.flags.seen);
            // Tree lines for the older messages of an expanded conversation.
            let last_child = info.child && !app.rows.get(pos + 1).is_some_and(|r| r.child && r.key == info.key);
            let (tree1, tree2) = match (info.child, last_child) {
                (false, _) => ("", ""),
                (true, false) => ("├ ", "│ "),
                (true, true) => ("└ ", "  "),
            };
            let tree = |t: &'static str| Span::styled(t, Style::new().fg(th.border));
            let dot = Span::styled(if unread { format!(" {} ", Icon::Unread.glyph()) } else { "   ".into() }, Style::new().fg(th.unread));
            let sender_style = if unread { Style::new().fg(th.text).bold() } else { Style::new().fg(th.subtext) };
            let subject_style = if unread { Style::new().fg(th.text) } else { Style::new().fg(th.overlay) };
            let mut marks: Vec<Span> = Vec::new();
            if any(&|m| m.flags.flagged) {
                marks.push(Span::styled(format!("{} ", Icon::Flagged.glyph()), Style::new().fg(th.flagged)));
            }
            if any(&|m| m.flags.answered) {
                marks.push(Span::styled(format!("{} ", Icon::Answered.glyph()), Style::new().fg(th.info)));
            }
            if any(&|m| m.has_attachments) {
                marks.push(Span::styled(format!("{} ", Icon::Attachment.glyph()), Style::new().fg(th.subtext)));
            }
            let date = Span::styled(short_date(e.timestamp), Style::new().fg(if unread { th.unread } else { th.overlay }));
            let subject = if e.subject.trim().is_empty() { "(no subject)".to_string() } else { e.subject.clone() };
            // Who: the conversation's participants and how many messages, or the sender.
            let mut who = vec![dot, tree(tree1)];
            if thread {
                who.push(Span::styled(crate::threads::participants(&app.envelopes, members, 3), sender_style));
                let n = members.len();
                let chevron = if info.expanded { "▾" } else { "▸" };
                who.push(Span::styled(format!(" {chevron}{n}"), Style::new().fg(th.accent2).bold()));
            } else if app.is_sent(i) {
                who.push(Span::styled(format!("{} ", Icon::Sent.glyph()), Style::new().fg(th.info)));
                who.push(Span::styled("me", Style::new().fg(th.info)));
            } else {
                who.push(Span::styled(e.sender().to_string(), sender_style));
            }
            let mut row = if two_line {
                Row::new(vec![
                    Cell::from(Text::from(vec![
                        Line::from(who),
                        Line::from(vec![Span::raw("   "), tree(tree2), Span::styled(subject, subject_style)]),
                    ])),
                    Cell::from(Text::from(vec![Line::from(date), Line::from(marks)]).right_aligned()),
                ])
                .height(2)
            } else {
                Row::new(vec![
                    Cell::from(Line::from(who)),
                    Cell::from(Span::styled(subject, subject_style)),
                    Cell::from(Line::from(marks).right_aligned()),
                    Cell::from(Line::from(date).right_aligned()),
                ])
            };
            if e.flags.deleted {
                row = row.style(Style::new().crossed_out());
            }
            row
        })
        .collect();
    let widths: Vec<Constraint> = if two_line {
        vec![Constraint::Fill(1), Constraint::Length(date_w)]
    } else {
        vec![Constraint::Percentage(28), Constraint::Fill(1), Constraint::Length(6), Constraint::Length(date_w)]
    };
    let table = Table::new(rows, widths).column_spacing(1).block(block).row_highlight_style(selected(focused));

    // The table state works on the clipped window.
    let mut window_state = ratatui::widgets::TableState::default().with_selected(
        app.list_state.selected().map(|s| s - offset),
    );
    f.render_stateful_widget(table, area, &mut window_state);
    *app.list_state.offset_mut() = offset;

    if app.visible.is_empty() {
        let msg = if let Some(e) = &app.sync_error {
            format!("sync failed: {e}\n\nCtrl-R to retry")
        } else if app.loading {
            "loading…".to_string()
        } else if app.filter.is_empty() {
            "empty mailbox".to_string()
        } else {
            "no message matches".to_string()
        };
        let style = if app.sync_error.is_some() { Style::new().fg(th.error) } else { Style::new().fg(th.overlay) };
        f.render_widget(Paragraph::new(msg).style(style).centered().wrap(Wrap { trim: true }), inner);
    }
}

fn draw_reader(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Reader;
    let ui_cfg = app.cfg.ui.clone();
    let image_pct = ui_cfg.image_pane_percent.clamp(10, 90);
    let cell = app.picker.as_ref().map(|p| (p.font_size().width, p.font_size().height)).unwrap_or((10, 20));
    let term_bg = app
        .picker
        .as_ref()
        .and_then(|p| {
            p.capabilities().iter().find_map(|c| match c {
                Capability::Background(r, g, b) => Some(orion_html::Color::Rgb(*r, *g, *b)),
                _ => None,
            })
        })
        .unwrap_or(orion_html::Color::Rgb(24, 24, 24));
    let has_picker = app.picker.is_some();
    let app_accounts = &app.accounts;
    let app_wide = app.wide;
    let Some(r) = app.reader.as_mut() else { return };
    let remote = r.remote;
    let (Some(view), Some(body)) = (&r.view, &mut r.body) else {
        let (msg, style) = match &r.error {
            Some(e) => (format!("could not open the message: {e}"), Style::new().fg(t().error)),
            None if r.unlocking => ("🔒 decrypting… touch the YubiKey if it blinks".to_string(), Style::new().fg(t().warn)),
            None => ("downloading message…".to_string(), Style::new().fg(t().overlay)),
        };
        let block = pane(" Message ".into(), focused);
        f.render_widget(Paragraph::new(msg).style(style).centered().block(block), area);
        return;
    };

    let (text_area, image_area) = if r.show_images && !r.images.is_empty() {
        let [a, b] =
            Layout::vertical([Constraint::Percentage(100 - image_pct), Constraint::Percentage(image_pct)]).areas(area);
        (a, Some(b))
    } else {
        (area, None)
    };

    // ---- headers
    let label = |k: &str| Span::styled(format!("{k:>11}  "), Style::new().fg(t().overlay));
    let mut lines: Vec<Line> = Vec::new();
    if r.show_headers {
        for l in view.raw_headers.lines() {
            match l.split_once(':') {
                Some((k, v)) if !l.starts_with([' ', '\t']) => {
                    lines.push(Line::from(vec![Span::styled(format!("{k}:"), Style::new().fg(t().accent)), Span::raw(v.to_string())]))
                }
                _ => lines.push(Line::raw(l.to_string())),
            }
        }
    } else {
        // Subject first, then who: "Name <address>" with the address faint.
        let subject = if view.subject.trim().is_empty() { "(no subject)".to_string() } else { view.subject.clone() };
        lines.push(Line::from(Span::styled(format!(" {subject}"), Style::new().fg(t().text).bold())));
        let (name, addr) = match view.from.rsplit_once('<') {
            Some((n, a)) if !n.trim().is_empty() => (n.trim().trim_matches('"').to_string(), format!("<{a}")),
            _ => (view.from.clone(), String::new()),
        };
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(name, Style::new().fg(t().accent2).bold()),
            Span::styled(format!(" {addr}"), Style::new().fg(t().overlay)),
        ]));
        let mut meta = vec![Span::styled(" to ", Style::new().fg(t().overlay)), Span::styled(view.to.clone(), Style::new().fg(t().subtext))];
        if !view.cc.is_empty() {
            meta.push(Span::styled("  cc ", Style::new().fg(t().overlay)));
            meta.push(Span::styled(view.cc.clone(), Style::new().fg(t().subtext)));
        }
        meta.push(Span::styled(format!("  ·  {}", view.date), Style::new().fg(t().overlay)));
        lines.push(Line::from(meta));
        if !view.reply_to.is_empty() && view.reply_to != view.from {
            lines.push(Line::from(vec![Span::styled(" reply to ", Style::new().fg(t().overlay)), Span::styled(view.reply_to.clone(), Style::new().fg(t().subtext))]));
        }
        lines.push(Line::raw(""));
    }
    let pane_imgs = body.pane_images(view);
    let shown: Vec<_> = view.attachments.iter().enumerate().filter(|(i, a)| !a.is_image() || pane_imgs.contains(i)).collect();
    if !shown.is_empty() {
        let mut spans = vec![label("Attachments")];
        for (n, (_, at)) in shown.iter().enumerate() {
            if n > 0 {
                spans.push(Span::raw("  "));
            }
            let icon = format!("{} ", Icon::Attachment.glyph());
            spans.push(Span::styled(format!("{icon}{} ({})", at.name, human_size(at.size())), Style::new().fg(t().accent2)));
        }
        lines.push(Line::from(spans));
    }
    if let Some(c) = &view.crypto {
        lines.extend(crypto_lines(c, &label));
    }
    if let Some(o) = &view.key_offer {
        let who = if o.uid.is_empty() { String::new() } else { format!(" · {}", o.uid) };
        lines.push(Line::from(vec![
            label("Key"),
            Span::styled(format!("sender's key available ({}): ", o.source.label()), Style::new().fg(t().warn)),
            Span::styled(orion_crypto::gpg::pretty_fingerprint(&o.fingerprint), Style::new().fg(t().accent).bold()),
            Span::raw(who).fg(t().overlay),
            Span::styled("  · I: import and use it for replies", Style::new().fg(t().warn)),
        ]));
    }
    if body.mode != BodyMode::Text && ui_cfg.remote_images != orion_core::config::RemoteImages::Never {
        let n = crate::app::App::remote_image_urls(view).len();
        let line = match remote {
            _ if n == 0 => None,
            crate::app::RemoteState::NotLoaded => Some((format!("{n} on the internet, not loaded"), "  · L: load (the sender will know you opened it)")),
            crate::app::RemoteState::Loading => Some(("downloading…".to_string(), "")),
            crate::app::RemoteState::Loaded => None,
        };
        if let Some((what, hint)) = line {
            lines.push(Line::from(vec![label("Images"), Span::raw(what), Span::raw(hint).fg(t().overlay)]));
        }
    }
    if view.html_doc.is_some() {
        let (now, other) = match body.mode {
            BodyMode::Html => ("HTML", "text · V: original"),
            BodyMode::Text => ("text", "HTML · V: original"),
            BodyMode::Original => ("original (drawn by Chromium)", "HTML · V: back"),
        };
        lines.push(Line::from(vec![label("View"), Span::raw(format!("{now}  ")), Span::raw(format!("(v: {other})")).fg(t().overlay)]));
    }
    let title = if app_wide {
        " Message ".to_string()
    } else {
        let account = app_accounts.get(r.mailbox.0).map(|a| a.name.as_str()).unwrap_or_default();
        let mailbox = app_accounts.get(r.mailbox.0).and_then(|a| a.mailboxes.iter().find(|m| m.id == r.mailbox.1)).map(|m| m.name.as_str()).unwrap_or(&r.mailbox.1);
        format!(" {account} · {mailbox} ")
    };
    let block = pane(title, focused);
    // One column of margin on each side of the text.
    let inner = block.inner(text_area);
    let inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    // A faint rule between the headers and the message.
    lines.push(Line::styled("─".repeat(inner.width as usize), Style::new().fg(t().selection_focused)));
    let header = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
    let header_h = header.line_count(inner.width) as u16;

    // ---- laid-out body
    body.ensure_layout(view, inner.width, cell, ui_cfg.max_image_rows, ui_cfg.link_markers);
    let body: &crate::body::Body = body;
    let rendered = body.rendered();
    let total = header_h as usize + rendered.lines.len();
    r.max_scroll = total.saturating_sub(inner.height as usize).min(u16::MAX as usize) as u16;
    r.scroll = r.scroll.min(r.max_scroll);
    r.page = inner.height.saturating_sub(2).max(1);
    let pct = if r.max_scroll == 0 { 100 } else { r.scroll as u32 * 100 / r.max_scroll as u32 };
    f.render_widget(block.title_bottom(Line::from(format!(" {pct}% ")).right_aligned()), text_area);

    let scroll = r.scroll;
    let (body_top, first_line) = if scroll < header_h {
        let h = (header_h - scroll).min(inner.height);
        f.render_widget(header.scroll((scroll, 0)), Rect { height: h, ..inner });
        (inner.y + h, 0usize)
    } else {
        (inner.y, (scroll - header_h) as usize)
    };
    let body_area = Rect { y: body_top, height: inner.bottom().saturating_sub(body_top), ..inner };
    if body_area.height > 0 {
        let visible: Vec<Line> = rendered
            .lines
            .iter()
            .skip(first_line)
            .take(body_area.height as usize)
            .map(|l| to_line(l, ui_cfg.html_colors, term_bg))
            .collect();
        f.render_widget(Paragraph::new(visible), body_area);

        // Inline images that show up in the viewport (even partially).
        let mut wanted = Vec::new();
        for p in &rendered.placements {
            let top = p.line as i64 - first_line as i64;
            if top + p.height as i64 <= 0 || top >= body_area.height as i64 {
                continue;
            }
            let key = (p.image, p.width, p.height);
            match body.inline.get(&key) {
                Some(InlineImage::Ready(proto)) => {
                    let pos = SignedPosition { x: p.x as i16, y: top as i16 };
                    f.render_widget(SlicedImage::new(proto, pos), body_area);
                }
                other => {
                    let msg = match other {
                        Some(InlineImage::Failed(e)) => format!("[image: {e}]"),
                        _ if has_picker => "[loading image…]".to_string(),
                        _ => "[image]".to_string(),
                    };
                    if top >= 0 {
                        let y = body_area.y + top as u16;
                        let rect = Rect::new(body_area.x + p.x, y, p.width.min(body_area.width.saturating_sub(p.x)), 1);
                        f.render_widget(Paragraph::new(msg).fg(t().overlay), rect);
                    }
                    if other.is_none() {
                        wanted.push(key);
                    }
                }
            }
        }
        if !wanted.is_empty() {
            app.request_inline_images(wanted);
        }
    }

    // ---- image attachments pane
    let Some(r) = app.reader.as_mut() else { return };
    let Some(view) = &r.view else { return };
    if let Some(img_area) = image_area {
        let att = r.images.get(r.image_idx).map(|&i| &view.attachments[i]);
        let title = match att {
            Some(a) => format!(" {} [{}/{}] ", a.name, r.image_idx + 1, r.images.len()),
            None => " image ".into(),
        };
        let block = pane(title, false);
        let inner = block.inner(img_area);
        f.render_widget(block, img_area);
        if let Some(slot) = &mut r.image {
            match &mut slot.state {
                Some(Ok(state)) => f.render_stateful_widget(StatefulImage::default().resize(Resize::Fit(None)), inner, state),
                Some(Err(e)) => f.render_widget(Paragraph::new(format!("could not decode: {e}")).fg(t().error), inner),
                None => f.render_widget(Paragraph::new("decoding…").fg(t().overlay), inner),
            }
        }
    }
}

fn draw_compose(f: &mut Frame, app: &mut App, area: Rect) {
    if app.compose.as_ref().is_some_and(|c| c.panel.is_some()) {
        return draw_compose_panel(f, app, area);
    }
    let Some(c) = &app.compose else { return };
    let account = app.accounts.get(c.account).map(|a| a.name.clone()).unwrap_or_default();
    let label = |k: &str| Span::styled(format!("{k:>11}  "), Style::new().fg(t().overlay));
    let d = &c.draft;
    let list = |v: &[String]| if v.is_empty() { "—".to_string() } else { v.join(", ") };
    let mut lines = vec![
        Line::from(vec![label("From"), Span::raw(d.from.clone()), Span::raw(format!("  (account {account})")).fg(t().overlay)]),
        Line::from(vec![label("To"), Span::raw(list(&d.to))]),
    ];
    if !d.cc.is_empty() {
        lines.push(Line::from(vec![label("Cc"), Span::raw(list(&d.cc))]));
    }
    if !d.bcc.is_empty() {
        lines.push(Line::from(vec![label("Bcc"), Span::raw(list(&d.bcc))]));
    }
    lines.push(Line::from(vec![label("Subject"), Span::raw(d.subject.clone()).bold()]));
    if !d.attachments.is_empty() {
        let names: Vec<String> = d.attachments.iter().map(|a| format!("📎 {} ({})", a.name, human_size(a.size()))).collect();
        lines.push(Line::from(vec![label("Attachments"), Span::styled(names.join("  "), Style::new().fg(t().accent2))]));
    }
    let on = |b: bool| if b { Span::styled("on ", Style::new().fg(t().ok).bold()) } else { Span::raw("off").fg(t().overlay) };
    lines.push(Line::from(vec![label("OpenPGP"), Span::raw("sign "), on(d.sign), Span::raw("  encrypt "), on(d.encrypt)]));
    if let Some(e) = &c.error {
        lines.push(Line::from(Span::styled(format!("  ✘ {e}"), Style::new().fg(t().error).bold())));
    }
    if let Some(i) = &c.info {
        lines.push(Line::from(Span::styled(format!("  {i}"), Style::new().fg(t().warn))));
    }
    for (addr, fpr, uid) in &c.untrusted {
        lines.push(Line::from(vec![
            Span::raw(format!("    {addr}  ")),
            Span::styled(orion_crypto::gpg::pretty_fingerprint(fpr), Style::new().fg(t().accent).bold()),
            Span::raw(format!("  {uid}")).fg(t().overlay),
        ]));
    }
    lines.push(Line::raw(""));
    for l in d.body.lines() {
        let style = if l.starts_with('>') { Style::new().fg(t().ok) } else { Style::new() };
        lines.push(Line::styled(l.to_string(), style));
    }
    let title = if c.sending { " Sending… ".to_string() } else { " New message — review ".to_string() };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(pane(title, true)), area);
}

fn crypto_lines(c: &CryptoReport, label: &dyn Fn(&str) -> Span<'static>) -> Vec<Line<'static>> {
    let green = Style::new().fg(t().ok);
    let yellow = Style::new().fg(t().warn);
    let red = Style::new().fg(t().error).bold();
    let mut out = Vec::new();
    if c.encrypted {
        let line = if c.decrypted {
            let mut t = format!("🔒 decrypted ({})", c.format);
            if let Some(loc) = &c.key_location {
                t.push_str(&format!(" with {loc}"));
            }
            Span::styled(t.replace('🔒', Icon::Encrypted.glyph()), green)
        } else {
            let (t, st) = match &c.problem {
                Some(CryptoProblem::NeedPin { prompt }) => (format!("🔒 encrypted — press Enter and enter the {prompt}"), yellow),
                Some(CryptoProblem::BadPin { retries: Some(n) }) => (format!("🔒 wrong PIN ({n} attempts left) — Enter to retry"), red),
                Some(CryptoProblem::BadPin { retries: None }) => ("🔒 wrong PIN or passphrase — Enter to retry".into(), red),
                Some(CryptoProblem::PinBlocked) => ("🔒 PIN blocked: unblock it with `gpg --card-edit` (admin → unblock)".into(), red),
                Some(CryptoProblem::CardMissing(card)) => (format!("🔒 plug in {card} and press Enter"), yellow),
                Some(CryptoProblem::NotConfirmed) => ("🔒 not confirmed: press Enter and touch the YubiKey when it blinks".into(), yellow),
                Some(CryptoProblem::WrongCard { needed, inserted }) => {
                    (format!("🔒 wrong key: this message is for {needed}, but the connected {inserted}. Plug in the right one and press Enter"), yellow)
                }
                Some(CryptoProblem::NoSecretKey(r)) => (format!("🔒 you do not have the secret key (encrypted to {})", r.join(", ")), red),
                Some(CryptoProblem::GpgMissing) => ("🔒 encrypted, but gpg is not installed".into(), red),
                Some(CryptoProblem::Failed(m)) => (format!("🔒 could not decrypt: {m} — Enter to retry"), red),
                None => ("🔒 encrypted".into(), yellow),
            };
            Span::styled(t.replace('🔒', Icon::Encrypted.glyph()), st)
        };
        out.push(Line::from(vec![label("Encryption"), line]));
    } else if let Some(CryptoProblem::Failed(m)) = &c.problem {
        out.push(Line::from(vec![label("Signature"), Span::styled(format!("✘ could not verify: {m}"), red)]));
    }
    for sig in &c.signatures {
        let trust = match sig.trust {
            Trust::Ultimate => "ultimate trust",
            Trust::Full => "full trust",
            Trust::Marginal => "marginal trust",
            Trust::Never => "key marked as NOT trusted",
            Trust::Undefined => "uncertified key",
        };
        let short = |f: &Option<String>| f.as_deref().map(|f| f[f.len().saturating_sub(16)..].to_string()).unwrap_or_else(|| sig.key_id.clone());
        let (t, st) = match sig.status {
            SigStatus::Good if matches!(sig.trust, Trust::Ultimate | Trust::Full) => (format!("✔ valid · {} · {trust}", sig.signer), green),
            SigStatus::Good if sig.trust == Trust::Never => (format!("✔ valid · {} · {trust}", sig.signer), red),
            SigStatus::Good => (format!("✔ valid · {} · {trust}", sig.signer), yellow),
            SigStatus::Bad => (format!("✘ INVALID: the message was altered · {}", sig.signer), red),
            SigStatus::Expired => (format!("✔ valid but expired · {}", sig.signer), yellow),
            SigStatus::ExpiredKey => (format!("✔ valid, expired key · {}", sig.signer), yellow),
            SigStatus::RevokedKey => (format!("⚠ REVOKED key · {}", sig.signer), red),
            SigStatus::UnknownKey => (format!("? cannot verify: missing public key {}", short(&sig.fingerprint)), yellow),
            SigStatus::Error => ("✘ error verifying the signature".to_string(), red),
        };
        out.push(Line::from(vec![label("Signature"), Span::styled(t, st)]));
    }
    out
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let proto = match &app.picker {
        Some(p) => format!("{:?}", p.protocol_type()).to_lowercase(),
        None => "no images".into(),
    };
    let left = match &app.mode {
        Mode::Search(q) => Line::from(vec![Span::styled("/", Style::new().fg(t().accent).bold()), Span::raw(q.clone()), Span::raw("█")]),
        Mode::Pin { prompt, pin, .. } => Line::from(vec![
            Span::styled(format!(" 🔑 {prompt}: "), Style::new().fg(t().warn).bold()),
            Span::raw("•".repeat(pin.chars().count())),
            Span::raw("█"),
            Span::raw("  (enter confirms, esc cancels)").fg(t().overlay),
        ]),
        Mode::ConfirmEmpty { prompt, .. } => Line::from(vec![
            Span::styled(format!(" 🗑 {prompt} "), Style::new().fg(t().error).bold()),
            Span::raw(" (y empties it, any other key cancels)").fg(t().overlay),
        ]),
        Mode::ConfirmDelete { prompt, .. } => Line::from(vec![
            Span::styled(format!(" 🗑 {prompt} "), Style::new().fg(t().error).bold()),
            Span::raw(" (y deletes, any other key cancels)").fg(t().overlay),
        ]),
        Mode::MoveTo { filter, .. } => Line::from(vec![
            Span::styled(" move to: ", Style::new().fg(t().accent).bold()),
            Span::raw(filter.clone()),
            Span::raw("█"),
            Span::raw("  (type to filter · ↑↓ · enter moves · esc cancels)").fg(t().overlay),
        ]),
        Mode::OpenLink(q) => Line::from(vec![
            Span::styled(" open link #: ", Style::new().fg(t().accent).bold()),
            Span::raw(q.clone()),
            Span::raw("█"),
            Span::raw("  (enter opens, esc cancels)").fg(t().overlay),
        ]),
        _ if !app.status.is_empty() => {
            let st = &app.status;
            let color = if st.starts_with("error") || st.contains("failed") { t().error } else { t().text };
            Line::from(vec![Span::styled(" ● ", Style::new().fg(t().accent)), Span::styled(st.clone(), Style::new().fg(color))])
        }
        _ if app.compose.as_ref().is_some_and(|c| c.picker.is_some()) => hints(&[
            ("type", "filter"),
            ("↑↓", "move"),
            ("enter", "open/attach"),
            ("space", "mark"),
            ("⌫", "up"),
            ("ctrl-t", "hidden"),
            ("esc", "cancel"),
        ]),
        _ if app.compose.as_ref().is_some_and(|c| c.panel.is_some()) => hints(&[
            ("tab", "next field"),
            ("ctrl-s", "send"),
            ("ctrl-a", "attach"),
            ("ctrl-g", "sign"),
            ("ctrl-k", "encrypt"),
            ("ctrl-e", "$EDITOR"),
            ("ctrl-q", "close"),
        ]),
        _ if app.compose.is_some() => hints(&[
            ("y", "send"),
            ("e", "edit"),
            ("a", "attach"),
            ("d", "drop attachment"),
            ("S", "sign"),
            ("E", "encrypt"),
            ("q", "close"),
        ]),
        _ => {
            let k = |a: Action| app.keymap.label(a);
            let in_reader = app.focus == Focus::Reader || (app.reader.is_some() && !app.wide);
            let mut list: Vec<(String, &str)> = vec![(k(Action::Help), "help")];
            if in_reader {
                list.extend([(k(Action::Reply), "reply"), (k(Action::Delete), "delete"), (k(Action::MoveTo), "move")]);
                if !app.saveable_attachments().is_empty() {
                    list.push((k(Action::SaveAttachments), "attachments"));
                }
                list.extend([(k(Action::ToggleView), "text/html"), (k(Action::Back), "back")]);
            } else {
                list.extend([(k(Action::Open), "open"), (k(Action::Compose), "write"), (k(Action::Search), "search"), (k(Action::Delete), "delete"), (k(Action::Quit), "quit")]);
            }
            let refs: Vec<(&str, &str)> = list.iter().map(|(a, b)| (a.as_str(), *b)).collect();
            hints(&refs)
        }
    };
    let busy: Vec<String> = app
        .accounts
        .iter()
        .filter(|a| !a.progress.is_empty())
        .map(|a| format!("{}: {}", a.name, a.progress))
        .collect();
    let busy = if busy.is_empty() { String::new() } else { format!("{} · ", busy.join(" · ")) };
    let right = Line::from(vec![
        Span::styled(busy, Style::new().fg(t().warn)),
        Span::styled(" orion ", Style::new().fg(t().surface).bg(t().accent).bold()),
        Span::styled(format!(" {proto} "), Style::new().fg(t().subtext).bg(t().selection)),
    ])
    .right_aligned();
    let right_w = (right.width() as u16).min(area.width / 2);
    let [l, r] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(right_w)]).areas(area);
    f.render_widget(left, l);
    f.render_widget(right, r);
}

/// The built-in compose panel: header fields, suggestions, Vim body.
fn draw_compose_panel(f: &mut Frame, app: &mut App, area: Rect) {
    use crate::compose_panel::Field;
    let th = t();
    let Some(c) = app.compose.as_mut() else { return };
    let account = app.accounts.get(c.account).map(|a| a.name.clone()).unwrap_or_default();
    let title = if c.sending { " Sending… ".to_string() } else { " New message ".to_string() };
    let block = pane(title, true);
    let inner = block.inner(area);
    let inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    f.render_widget(block, area);
    let Some(p) = c.panel.as_mut() else { return };
    let label_w = 10usize;
    let label = |k: &str, focused: bool| {
        Span::styled(format!("{k:<label_w$}"), if focused { Style::new().fg(th.accent).bold() } else { Style::new().fg(th.overlay) })
    };

    // ---- header
    let mut lines: Vec<Line> = vec![Line::from(vec![
        label("From", false),
        Span::styled(c.draft.from.clone(), Style::new().fg(th.subtext)),
        Span::styled(format!("  · {account}"), Style::new().fg(th.overlay)),
    ])];
    let mut field_row: Vec<(Field, usize)> = Vec::new();
    for field in [Field::To, Field::Cc, Field::Bcc] {
        let focused = p.field == field;
        field_row.push((field, lines.len()));
        let mut spans = vec![label(field.label(), focused)];
        for a in p.list(field) {
            spans.push(Span::styled(format!(" {a} "), Style::new().fg(th.text).bg(th.selection)));
            spans.push(Span::raw(" "));
        }
        if focused {
            spans.push(Span::styled(p.input.clone(), Style::new().fg(th.text)));
            spans.push(Span::styled("█", Style::new().fg(th.accent)));
        }
        lines.push(Line::from(spans));
    }
    let subject_focused = p.field == Field::Subject;
    let mut subject = vec![label("Subject", subject_focused), Span::styled(p.subject.clone(), Style::new().fg(th.text).bold())];
    if subject_focused {
        subject.push(Span::styled("█", Style::new().fg(th.accent)));
    }
    lines.push(Line::from(subject));
    // Always shown, so it is clear how to attach.
    let mut spans = vec![label("Attach", false)];
    if c.draft.attachments.is_empty() {
        spans.push(Span::styled("none", Style::new().fg(th.overlay)));
    }
    for a in &c.draft.attachments {
        spans.push(Span::styled(format!("{} {} ({})  ", Icon::Attachment.glyph(), a.name, human_size(a.size())), Style::new().fg(th.accent2)));
    }
    spans.push(Span::styled("  ctrl-a", Style::new().fg(th.accent2).bold()));
    spans.push(Span::styled(" add", Style::new().fg(th.overlay)));
    if !c.draft.attachments.is_empty() {
        spans.push(Span::styled("  ctrl-x", Style::new().fg(th.accent2).bold()));
        spans.push(Span::styled(" remove last", Style::new().fg(th.overlay)));
    }
    lines.push(Line::from(spans));
    let on = |b: bool, what: &str| {
        if b { Span::styled(format!("{what} ✓  "), Style::new().fg(th.ok).bold()) } else { Span::styled(format!("{what} ✗  "), Style::new().fg(th.overlay)) }
    };
    lines.push(Line::from(vec![
        label("OpenPGP", false),
        Span::styled(format!("{} ", Icon::Encrypted.glyph()), Style::new().fg(th.subtext)),
        on(c.draft.sign, "sign"),
        on(c.draft.encrypt, "encrypt"),
    ]));
    if let Some(e) = &c.error {
        lines.push(Line::styled(format!("✘ {e}"), Style::new().fg(th.error).bold()));
    }
    if let Some(i) = &c.info {
        lines.push(Line::styled(i.clone(), Style::new().fg(th.warn)));
    }
    for (addr, fpr, uid) in &c.untrusted {
        lines.push(Line::from(vec![
            Span::raw(format!("  {addr}  ")),
            Span::styled(orion_crypto::gpg::pretty_fingerprint(fpr), Style::new().fg(th.accent).bold()),
            Span::styled(format!("  {uid}"), Style::new().fg(th.overlay)),
        ]));
    }
    let header = Paragraph::new(lines).wrap(Wrap { trim: false });
    let header_h = (header.line_count(inner.width) as u16).min(inner.height.saturating_sub(3));
    let [head, rule, body] = Layout::vertical([Constraint::Length(header_h), Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
    f.render_widget(header, head);
    let mode = match p.body.mode {
        edtui::EditorMode::Insert => " INSERT ",
        edtui::EditorMode::Visual => " VISUAL ",
        edtui::EditorMode::Search => " SEARCH ",
        edtui::EditorMode::Normal => " NORMAL ",
    };
    let body_focused = p.field == Field::Body;
    let rule_line = if body_focused {
        Line::from(vec![
            Span::styled(mode, Style::new().fg(th.surface).bg(th.accent).bold()),
            Span::styled("─".repeat(rule.width.saturating_sub(mode.len() as u16) as usize), Style::new().fg(th.selection_focused)),
        ])
    } else {
        Line::styled("─".repeat(rule.width as usize), Style::new().fg(th.selection_focused))
    };
    f.render_widget(rule_line, rule);

    // ---- body: the Vim-like editor
    let theme = edtui::EditorTheme::default()
        .base(Style::new().fg(th.text))
        .cursor_style(if body_focused { Style::new().fg(th.surface).bg(th.accent) } else { Style::new().fg(th.text) })
        .selection_style(Style::new().bg(th.selection_focused))
        .hide_status_line();
    f.render_widget(edtui::EditorView::new(&mut p.body).theme(theme).wrap(true), body);

    // ---- suggestions under the address field being typed
    if let Some(&(_, row)) = field_row.iter().find(|(fl, _)| *fl == p.field)
        && !p.suggestions.is_empty()
    {
        let w = p.suggestions.iter().map(|s| s.display().chars().count()).max().unwrap_or(20).clamp(30, 70) as u16 + 4;
        let h = p.suggestions.len() as u16 + 2;
        let y = head.y + row as u16 + 1;
        let rect = Rect::new(head.x + label_w as u16, y, w.min(inner.width.saturating_sub(label_w as u16)), h.min(area.bottom().saturating_sub(y)));
        let items: Vec<Line> = p
            .suggestions
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let name = format!(" {}", if s.name.is_empty() { s.email.clone() } else { s.name.clone() });
                let email = if s.name.is_empty() { String::new() } else { format!("  {}", s.email) };
                let pad = (rect.width as usize).saturating_sub(2 + name.chars().count() + email.chars().count());
                let line = Line::from(vec![
                    Span::styled(name, Style::new().fg(th.text).bold()),
                    Span::styled(email, Style::new().fg(if i == p.sel { th.subtext } else { th.overlay })),
                    Span::raw(" ".repeat(pad)),
                ]);
                if i == p.sel { line.style(selected(true)) } else { line }
            })
            .collect();
        f.render_widget(Clear, rect);
        f.render_widget(Paragraph::new(items).style(popup_bg()).block(pane(String::new(), true)), rect);
    }
}

/// Key hints: the key highlighted, what it does faint.
fn hints(list: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, (key, what)) in list.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::new()));
        }
        spans.push(Span::styled(key.to_string(), Style::new().fg(t().accent2).bold()));
        spans.push(Span::styled(format!(" {what}"), Style::new().fg(t().overlay)));
    }
    Line::from(spans)
}

/// The open message's attachments, for `s`.
fn draw_attachments_popup(f: &mut Frame, app: &App, sel: usize) {
    let list = app.saveable_attachments();
    let area = f.area();
    let rows: Vec<(String, String)> = list.iter().map(|a| (a.name.clone(), human_size(a.size()))).collect();
    // Wide enough for the hints; names fill the row so the highlight does too.
    let longest = rows.iter().map(|r| r.0.chars().count()).max().unwrap_or(10).min(60);
    let w = ((longest + 16) as u16).max(52).min(area.width);
    let name_w = (w as usize).saturating_sub(16);
    let h = (rows.len() as u16 + 4).min(area.height);
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    let mut lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(i, (name, size))| {
            let name: String = if name.chars().count() > name_w { name.chars().take(name_w - 1).chain(['…']).collect() } else { name.clone() };
            let line = Line::from(vec![
                Span::styled(format!(" {} ", Icon::Attachment.glyph()), Style::new().fg(t().subtext)),
                Span::styled(format!("{name:<name_w$}"), Style::new().fg(t().text)),
                Span::styled(format!("{size:>9} "), Style::new().fg(t().overlay)),
            ]);
            if i == sel { line.style(selected(true)) } else { line }
        })
        .collect();
    lines.push(Line::raw(""));
    lines.push(hints(&[("enter", "save"), ("a", "save all"), ("o", "open"), ("esc", "close")]));
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).style(popup_bg()).block(pane(" Attachments → ~/Downloads ".into(), true)), rect);
}

/// Folder chooser for `m`.
fn draw_move_popup(f: &mut Frame, targets: &[(String, String)], sel: usize) {
    let area = f.area();
    let w = (targets.iter().map(|t| t.1.chars().count()).max().unwrap_or(20) as u16 + 8).clamp(30, area.width);
    let h = (targets.len() as u16 + 2).clamp(3, area.height.saturating_sub(4).max(3));
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height.saturating_sub(h)) / 2, w, h);
    let rows = h.saturating_sub(2) as usize;
    let first = sel.saturating_sub(rows.saturating_sub(1));
    let lines: Vec<Line> = if targets.is_empty() {
        vec![Line::raw(" no folder matches").fg(t().overlay)]
    } else {
        targets
            .iter()
            .enumerate()
            .skip(first)
            .take(rows)
            .map(|(i, (_, name))| {
                let line = Line::raw(format!(" {name}"));
                if i == sel { line.style(Style::new().bg(t().selection_focused).bold()) } else { line }
            })
            .collect()
    };
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).style(popup_bg()).block(pane(" Move to ".into(), true)), rect);
}

/// The attachment file browser, over the compose screen.
fn draw_file_picker(f: &mut Frame, p: &crate::filepicker::FilePicker) {
    let area = f.area();
    let w = (area.width * 4 / 5).max(40).min(area.width);
    let h = (area.height * 4 / 5).max(10).min(area.height.saturating_sub(1));
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height.saturating_sub(1) - h) / 2, w, h);
    let block = pane(format!(" Attach — {} ", p.dir.display()), true);
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block.style(popup_bg()), rect);
    let [top, list_area, bottom] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(inner);
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled("filter or path: ", Style::new().fg(t().accent).bold()), Span::raw(p.filter.clone()), Span::raw("█")])),
        top,
    );
    let items = p.visible();
    let rows = list_area.height as usize;
    let first = p.sel.saturating_sub(rows.saturating_sub(1));
    let name_w = (list_area.width as usize).saturating_sub(14);
    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, e)| {
            let marked = !e.dir && p.marked.contains(&p.dir.join(&e.name));
            let (name, style) = if e.name.is_empty() {
                ("../".to_string(), Style::new().fg(t().accent))
            } else if e.dir {
                (format!("{}/", e.name), Style::new().fg(t().accent).bold())
            } else {
                (e.name.clone(), Style::new())
            };
            let name: String = if name.chars().count() > name_w { name.chars().take(name_w.saturating_sub(1)).chain(['…']).collect() } else { name };
            let size = if e.dir { String::new() } else { crate::filepicker::human_size(e.size) };
            let mut line = Line::from(vec![
                Span::raw(if marked { "● " } else { "  " }).fg(t().ok),
                Span::styled(format!("{name:<name_w$}"), style),
                Span::raw(format!("{size:>10}")).fg(t().overlay),
            ]);
            if i == p.sel {
                line = line.style(Style::new().bg(t().selection_focused));
            }
            line
        })
        .collect();
    f.render_widget(Paragraph::new(lines), list_area);
    let status = match (&p.error, p.marked.len()) {
        (Some(e), _) => Span::raw(e.clone()).fg(t().error),
        (None, 0) if items.is_empty() && !p.filter.is_empty() => Span::raw("no match — a path starting with / or ~ is also accepted").fg(t().overlay),
        (None, 0) => Span::raw(format!("{} item(s)", items.len())).fg(t().overlay),
        (None, n) => Span::raw(format!("{n} marked — enter attaches them")).fg(t().ok),
    };
    f.render_widget(Paragraph::new(Line::from(status)), bottom);
}

fn draw_popup(f: &mut Frame, title: &str, lines: &[String]) {
    let area = f.area();
    let w = (lines.iter().map(|l| l.chars().count()).max().unwrap_or(20) as u16 + 4).max(title.chars().count() as u16 + 4).min(area.width);
    let h = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    let text: Vec<Line> = lines.iter().map(|l| Line::raw(l.clone())).collect();
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(text).style(popup_bg()).block(pane(title.into(), true)), rect);
}

fn draw_help(f: &mut Frame, app: &App) {
    let area = f.area();
    let rows: Vec<(String, &str)> = Action::ALL
        .iter()
        .map(|a| (app.keymap.labels.get(a.name()).map(|v| v.join(" ")).unwrap_or_default(), a.description()))
        .collect();
    let key_w = rows.iter().map(|r| r.0.chars().count()).max().unwrap_or(10);
    let desc_w = rows.iter().map(|r| r.1.chars().count()).max().unwrap_or(40);
    let w = (key_w + desc_w + 7) as u16;
    let w = w.min(area.width);
    let h = (rows.len() as u16 + 2).min(area.height);
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(keys, desc)| {
            Line::from(vec![
                Span::styled(format!(" {keys:>key_w$}  "), Style::new().fg(t().accent2).bold()),
                Span::styled(desc.to_string(), Style::new().fg(t().text)),
            ])
        })
        .collect();
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).style(popup_bg()).block(pane(" Keys · any key closes ".into(), true)), rect);
}

/// Background of floating windows: a shade darker than the panes.
fn popup_bg() -> Style {
    Style::new().bg(t().surface)
}

#[cfg(test)]
mod smoke {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_image::picker::Picker;

    use orion_core::config::{AccountConfig, AccountKind, Config};

    use crate::app::{Account, App, Focus};
    use crate::keys::Keymap;
    use crate::worker::{self, Event};

    struct Harness {
        app: App,
        rx: mpsc::Receiver<Event>,
    }

    impl Harness {
        fn new() -> Self {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/demo.mbox");
            let acc = AccountConfig::new("demo", AccountKind::Mbox { path });
            let (tx, rx) = mpsc::channel();
            let cfg = Config::default();
            let (keymap, _) = Keymap::new(&cfg.keys);
            let accounts = vec![Account {
                name: acc.name.clone(),
                tx: worker::spawn(0, acc, None, None, tx.clone()),
                mailboxes: Vec::new(),
                error: None,
                progress: String::new(),
            }];
            Harness { app: App::new(cfg, keymap, Some(Picker::halfblocks()), tx, accounts), rx }
        }

        /// Processes events until the condition holds (or fails after 5 s).
        fn until(&mut self, what: &str, cond: impl Fn(&App) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !cond(&self.app) {
                let left = deadline.saturating_duration_since(Instant::now());
                match self.rx.recv_timeout(left) {
                    Ok(ev) => self.app.on_event(ev),
                    Err(_) => panic!("timed out waiting for: {what}"),
                }
            }
        }

        fn key(&mut self, c: KeyCode) {
            self.app.on_key(KeyEvent::new(c, KeyModifiers::NONE));
        }

        fn screen(&mut self) -> String {
            let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
            t.draw(|f| super::draw(f, &mut self.app)).unwrap();
            let buf = t.backend().buffer();
            (0..buf.area.height)
                .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    impl Harness {
        fn select_subject(&mut self, subject: &str) {
            let pos = self.app.visible.iter().position(|&i| self.app.envelopes[i].subject == subject).expect(subject);
            self.app.list_state.select(Some(pos));
        }
    }

    #[test]
    fn three_pane_preview() {
        let mut h = Harness::new();
        h.until("envelopes", |a| a.envelopes.len() == 4);
        h.app.wide = true;
        h.app.focus = Focus::List;
        h.app.cfg.ui.mark_read_after = 1;
        // Arrive at an unread message by moving the selection onto it.
        let unread = h.app.visible.iter().position(|&i| !h.app.envelopes[i].flags.seen).unwrap();
        if unread == 0 {
            h.app.list_state.select(Some(1));
            h.key(KeyCode::Char('k'));
        } else {
            h.app.list_state.select(Some(unread - 1));
            h.key(KeyCode::Char('j'));
        }
        let id = h.app.envelopes[h.app.selected_envelope().unwrap()].id;
        h.until("previewed", |a| a.reader.as_ref().is_some_and(|r| r.id == id && r.view.is_some()));
        assert_eq!(h.app.focus, Focus::List, "a preview keeps the focus on the list");
        let seen = |a: &App| a.envelopes.iter().find(|e| e.id == id).unwrap().flags.seen;
        assert!(!seen(&h.app), "not read yet");
        h.until("read after the delay", |a| seen(a));
        // Enter goes into it; Esc comes back to the list and keeps it shown.
        h.key(KeyCode::Enter);
        assert_eq!(h.app.focus, Focus::Reader);
        h.key(KeyCode::Esc);
        assert_eq!(h.app.focus, Focus::List);
        assert!(h.app.reader.is_some());
    }

    #[test]
    fn html_newsletter() {
        let mut h = Harness::new();
        h.app.cfg.ui.open_command = "true".into(); // do not open a real browser
        h.until("list", |a| a.envelopes.len() == 4 && !a.loading);
        h.select_subject("Boletín HTML de prueba");
        h.key(KeyCode::Enter);
        h.until("message", |a| a.reader.as_ref().is_some_and(|r| r.view.is_some()));
        let s = h.screen(); // the first pass lays out and requests the inline image
        h.until("inline logo encoded", |a| {
            a.reader.as_ref().and_then(|r| r.body.as_ref()).is_some_and(|b| b.inline.values().any(|i| matches!(i, crate::body::InlineImage::Ready(_))))
        });
        let s2 = h.screen();
        println!("{s}\n{s2}");
        assert!(s.contains("Novedades de noviembre"));
        assert!(!s.contains("previsualización"), "the hidden preheader is not shown");
        assert!(s.contains("View  HTML"));
        let cols = s.lines().find(|l| l.contains("Columna izquierda")).expect("columns");
        assert!(cols.contains("Columna derecha"), "columns side by side");
        assert!(s.contains("enlaces numerados[1]"));
        assert!(!s.contains("logo.png"), "the embedded image is not listed as an attachment");
        assert!(!s2.contains("loading image"));

        // Scroll down to the table and the links footer.
        h.key(KeyCode::Char('G'));
        let s = h.screen();
        println!("{s}");
        assert!(s.contains("Tostada con tomate │ 2,50 €"));
        assert!(s.contains("[1] https://example.com/novedades"));

        // Open link 1.
        h.key(KeyCode::Char('o'));
        h.key(KeyCode::Char('1'));
        h.key(KeyCode::Enter);
        assert_eq!(h.app.status, "opening https://example.com/novedades");
        h.key(KeyCode::Char('o'));
        h.key(KeyCode::Char('9'));
        h.key(KeyCode::Enter);
        assert_eq!(h.app.status, "link 9 does not exist");

        // Switch to plain text.
        h.key(KeyCode::Char('v'));
        let s = h.screen();
        assert!(s.contains("versión texto") && s.contains("View  text"));
    }

    #[test]
    fn plain_and_attachment_image() {
        let mut h = Harness::new();
        h.until("list", |a| a.envelopes.len() == 4 && !a.loading);
        h.select_subject("Foto del degradado");
        h.key(KeyCode::Enter);
        assert_eq!(h.app.focus, Focus::Reader);
        assert!(h.screen().contains("downloading message"));
        h.until("attached image decoded", |a| {
            a.reader.as_ref().and_then(|r| r.image.as_ref()).is_some_and(|i| matches!(i.state, Some(Ok(_))))
        });
        let s = h.screen();
        println!("{s}");
        assert!(s.contains("degradado.png"));
        // It has an HTML part: HTML is shown; with "v" the text part with >From unescaped.
        h.key(KeyCode::Char('v'));
        assert!(h.screen().contains("From the archive"), "must unescape >From");

        // Next (older): HTML-only message.
        h.key(KeyCode::Char('n'));
        h.until("html message", |a| a.reader.as_ref().is_some_and(|r| r.view.is_some()));
        let s = h.screen();
        println!("{s}");
        assert!(s.contains("Novedades") && s.contains("solo HTML"));
        assert!(s.contains("• Uno"));

        // Flag as important (mbox: in memory only, with a warning).
        h.key(KeyCode::Char('F'));
        let id = h.app.reader.as_ref().unwrap().id;
        assert!(h.app.envelopes.iter().find(|e| e.id == id).unwrap().flags.flagged);
        h.until("read-only warning", |a| a.status.contains("read-only"));

        // Filter.
        h.key(KeyCode::Esc);
        h.key(KeyCode::Char('/'));
        for c in "bienv".chars() {
            h.key(KeyCode::Char(c));
        }
        h.key(KeyCode::Enter);
        assert_eq!(h.app.visible.len(), 1);
    }
}

/// Screenshots for design work: `ORION_SHOT_DIR=dir cargo test -p orion-tui
/// screenshots -- --ignored` writes the screen of a few states as JSON cells
/// (`scripts/render_shot.py` turns them into PNG).
#[cfg(test)]
mod shots {
    use std::sync::mpsc;

    use ratatui::backend::TestBackend;
    use ratatui::style::{Color, Modifier};
    use ratatui::Terminal;

    use orion_core::config::Config;
    use orion_core::model::{Envelope, Flags, MailboxInfo, MessageId};

    use crate::app::{Account, App, Focus};
    use crate::keys::Keymap;
    use crate::worker::Event;

    fn color(c: Color) -> serde_json::Value {
        match c {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}").into(),
            Color::Reset => serde_json::Value::Null,
            other => format!("{other:?}").into(),
        }
    }

    fn dump(app: &mut App, w: u16, h: u16, path: &std::path::Path) {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| super::draw(f, app)).unwrap();
        let buf = t.backend().buffer();
        let rows: Vec<Vec<serde_json::Value>> = (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| {
                        let c = &buf[(x, y)];
                        serde_json::json!([c.symbol(), color(c.fg), color(c.bg), c.modifier.contains(Modifier::BOLD), c.modifier.contains(Modifier::DIM), c.modifier.contains(Modifier::CROSSED_OUT)])
                    })
                    .collect()
            })
            .collect();
        std::fs::write(path, serde_json::to_string(&rows).unwrap()).unwrap();
    }

    fn mb(id: &str, total: usize, unread: usize) -> MailboxInfo {
        MailboxInfo { id: id.into(), name: id.into(), total, unread }
    }

    #[test]
    #[ignore]
    fn screenshots() {
        let Ok(dir) = std::env::var("ORION_SHOT_DIR") else { return };
        let dir = std::path::PathBuf::from(dir);
        let (tx, _rx) = mpsc::channel();
        let cfg = Config::default();
        let (keymap, _) = Keymap::new(&cfg.keys);
        let account = |name: &str| {
            let (rtx, rrx) = mpsc::channel();
            std::mem::forget(rrx); // requests go nowhere
            Account { name: name.into(), tx: rtx, mailboxes: Vec::new(), error: None, progress: String::new() }
        };
        let accounts = vec![account("work"), account("gmail"), account("home")];
        let mut app = App::new(cfg, keymap, None, tx, accounts);
        let lists = [
            vec![mb("INBOX", 7969, 3), mb("Drafts", 2, 0), mb("Sent", 310, 0), mb("Junk", 101, 101), mb("Deleted", 40, 0), mb("Archivo", 900, 0), mb("clientes", 12, 0), mb("clientes/2025", 7, 0)],
            vec![mb("INBOX", 4200, 1), mb("[Gmail]/Sent Mail", 800, 0), mb("[Gmail]/Spam", 29, 29), mb("[Gmail]/Trash", 27, 0), mb("Recibos", 140, 0), mb("Viaje", 33, 0)],
            vec![mb("INBOX", 51, 0), mb("Sent", 20, 0), mb("Trash", 3, 0)],
        ];
        for (i, list) in lists.into_iter().enumerate() {
            app.on_event(Event::Mailboxes { account: i, list });
        }
        app.accounts[1].progress = "syncing INBOX…".into();
        let now = chrono::Local::now().timestamp();
        let env = |id: u64, name: &str, addr: &str, subject: &str, ago: i64, seen: bool, flagged: bool, answered: bool, att: bool| Envelope {
            id: MessageId(id),
            from_name: name.into(),
            from_addr: addr.into(),
            subject: subject.into(),
            timestamp: now - ago,
            date: String::new(),
            flags: Flags { seen, flagged, answered, ..Default::default() },
            size: 1000,
            has_attachments: att,
            message_id: String::new(),
            parents: Vec::new(),
        };
        let h = 3600;
        let envelopes = vec![
            env(1, "Amazon.es", "envios@amazon.es", "Tu pedido de «Teclado mecánico» ha sido enviado", 600, false, false, false, false),
            env(2, "Ana García", "ana@example.org", "Re: Cena del sábado — ¿confirmamos a las 21:00?", 2 * h, false, true, false, false),
            env(3, "NVIDIA GeForce", "gaming@nvgaming.nvidia.com", "Ilumina las fiestas con NVIDIA", 26 * h, true, false, false, false),
            env(4, "Dana Ruiz", "dana@example.net", "Contrato firmado (PDF adjunto)", 30 * h, false, false, true, true),
            env(5, "ASUS Spain", "noreply@nedm.asus.com", "¡Ya está aquí el Black Friday de ASUS!", 3 * 24 * h, true, false, false, false),
            env(6, "GitHub", "noreply@github.com", "[orion] CI passed on main", 4 * 24 * h, true, false, false, false),
            env(7, "Luis Martín", "luis@example.net", "Fotos del viaje a Asturias", 12 * 24 * h, true, true, true, true),
            env(8, "Banco Ejemplo", "avisos@banco.example", "Tu extracto de noviembre ya está disponible", 40 * 24 * h, true, false, false, true),
            env(9, "Renfe", "venta@renfe.example", "Billete Madrid → Oviedo", 400 * 24 * h, true, false, false, true),
        ];
        // A conversation: Ana wrote, Luis answered, Ana answered again.
        let mut envelopes = envelopes;
        envelopes[1].message_id = "cena-3".into();
        envelopes[1].parents = vec!["cena-1".into(), "cena-2".into()];
        envelopes.push(Envelope { message_id: "cena-2".into(), parents: vec!["cena-1".into()], ..env(10, "Luis Martín", "luis@example.net", "Re: Cena del sábado", 5 * h, true, false, false, false) });
        envelopes.push(Envelope { message_id: "cena-1".into(), ..env(11, "Ana García", "ana@example.org", "Cena del sábado", 50 * h, true, false, true, false) });
        app.current = Some((0, "INBOX".into()));
        app.on_event(Event::Envelopes { account: 0, mailbox: "INBOX".into(), envelopes, from_cache: false });
        app.list_state.select(Some(1));
        app.focus = Focus::List;
        dump(&mut app, 150, 40, &dir.join("list.json"));
        // Your reply, from Sent, joins the conversation.
        let mine = Envelope { message_id: "cena-4".into(), parents: vec!["cena-1".into(), "cena-2".into()], ..env(1, "Me", "me@example.com", "Re: Cena del sábado", 4 * h, true, false, false, false) };
        app.on_event(Event::Related { account: 0, mailbox: "Sent".into(), envelopes: vec![mine] });
        app.list_state.select(Some(1));
        app.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('t'), crossterm::event::KeyModifiers::NONE));
        dump(&mut app, 150, 40, &dir.join("thread.json"));
        app.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('t'), crossterm::event::KeyModifiers::NONE));

        // The reader, with a signed and encrypted message.
        app.list_state.select(Some(3));
        app.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Enter, crossterm::event::KeyModifiers::NONE));
        let raw = "From: Dana Ruiz <dana@example.net>\r\nTo: me@example.com\r\nCc: Ana García <ana@example.org>\r\nSubject: Contrato firmado (PDF adjunto)\r\nDate: Thu, 20 Nov 2025 18:12:00 +0100\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHola,\r\n\r\nTe adjunto el contrato ya firmado por las dos partes. Si ves algo raro\r\ndímelo antes del viernes y lo corregimos.\r\n\r\n> ¿Me lo puedes mandar firmado?\r\n> Gracias.\r\n\r\nUn abrazo,\r\nDana\r\n--b\r\nContent-Type: application/pdf; name=contrato.pdf\r\nContent-Disposition: attachment; filename=contrato.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--b--\r\n";
        let mut view = orion_core::parse::message_view(raw.as_bytes()).unwrap();
        view.crypto = Some(orion_core::model::CryptoReport {
            encrypted: true,
            decrypted: true,
            format: "PGP/MIME",
            key_location: Some("YubiKey #12345678".into()),
            signatures: vec![orion_core::model::SignatureInfo {
                status: orion_core::model::SigStatus::Good,
                key_id: "0123456789ABCDEF".into(),
                fingerprint: None,
                signer: "Dana Ruiz <dana@example.net>".into(),
                trust: orion_core::model::Trust::Ultimate,
                created: None,
            }],
            ..Default::default()
        });
        let id = app.reader.as_ref().unwrap().id;
        app.on_event(Event::Message { account: 0, mailbox: "INBOX".into(), id, result: Ok(Box::new(view)) });
        app.focus = Focus::Reader;
        dump(&mut app, 150, 40, &dir.join("reader.json"));
        app.mode = crate::app::Mode::MoveTo { id, key: (0, "INBOX".into()), filter: "ar".into(), sel: 0 };
        dump(&mut app, 150, 40, &dir.join("move.json"));
        // The compose panel, typing "lu" in To.
        app.mode = crate::app::Mode::Normal;
        app.contacts = vec![
            crate::contacts::Contact { name: "Lucía Pérez".into(), email: "lucia@example.com".into(), score: 40 },
            crate::contacts::Contact { name: "Luis Martín".into(), email: "luis@example.net".into(), score: 30 },
            crate::contacts::Contact { name: "Ana García".into(), email: "ana@example.org".into(), score: 50 },
        ];
        let mut draft = orion_core::compose::new_message("Me <me@example.com>", Some("Me"));
        draft.to = vec!["Ana García <ana@example.org>".into()];
        draft.subject = "Cena del sábado".into();
        draft.body = "Hola,\n\n¿Confirmamos a las 21:00?\n\n-- \nMe".into();
        draft.sign = true;
        let mut panel = crate::compose_panel::Panel::new(&draft);
        panel.field = crate::compose_panel::Field::To;
        let book = app.contacts.clone();
        for ch in "lu".chars() {
            panel.key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char(ch), crossterm::event::KeyModifiers::NONE), &book);
        }
        app.compose = Some(crate::compose_ui::Compose {
            account: 0,
            draft,
            text: String::new(),
            path: std::path::PathBuf::from("/dev/null"),
            error: None,
            info: None,
            sending: false,
            ticket: 0,
            encrypt_touched: false,
            missing_keys: Vec::new(),
            untrusted: Vec::new(),
            picker: None,
            panel: Some(panel),
            confirm_discard: false,
        });
        dump(&mut app, 150, 40, &dir.join("compose.json"));
        app.compose = None;
        app.mode = crate::app::Mode::Attachments { sel: 0 };
        dump(&mut app, 150, 40, &dir.join("attachments.json"));
        app.mode = crate::app::Mode::Help;
        dump(&mut app, 150, 48, &dir.join("help.json"));
    }
}

#[cfg(test)]
mod original_view_fit {
    use ratatui_image::picker::{Picker, ProtocolType};

    /// The original view's image must never be wider than its placement
    /// (a wider protocol image is cut on the right: the page looks off-centre).
    #[test]
    #[allow(deprecated)] // a fixed font size is exactly what this test needs
    fn snapshot_fits_its_placement() {
        let mut picker = Picker::from_fontsize(ratatui_image::FontSize { width: 21, height: 46 });
        picker.set_protocol_type(ProtocolType::Kitty);
        let doc = orion_html::Doc::parse(r#"<img src="cid:orion-original">"#);
        for (pane_cols, snap) in [(62u16, (1302u32, 5000u32)), (62, (3150, 9000)), (150, (3150, 9000)), (62, (900, 20000))] {
            let px = |_: usize| Some(snap);
            let opts = orion_html::LayoutOptions { cell: (21, 46), max_image_rows: u16::MAX, link_markers: false, image_px: &px };
            let r = doc.layout(pane_cols, &opts);
            let p = &r.placements[0];
            let img = image::DynamicImage::new_rgb8(snap.0, snap.1);
            let proto = ratatui_image::sliced::SlicedProtocol::new_with_resize(&picker, img, ratatui::layout::Size::new(p.width, p.height), ratatui_image::Resize::Fit(None)).unwrap();
            let size = proto.size();
            assert!(p.x + p.width <= pane_cols, "placement inside the pane: {p:?} in {pane_cols}");
            assert!(size.width <= p.width && size.height <= p.height, "pane {pane_cols}, snapshot {snap:?}: protocol {size:?} vs placement {}x{}", p.width, p.height);
        }
    }
}
