//! Rendering. Colors are ANSI palette entries on purpose: herdr runs with `theme = "terminal"`,
//! so the panel follows whatever palette the outer terminal uses.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::app::{Act, App, Mode, Place, Row};
use super::data::now;

const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Every status glyph on the board. Nerd Font glyphs draw wider than their cell, hence the
/// trailing space on those followed by a count. `WORKTREES_ICONS=plain` = Unicode only.
struct Icons {
    repo: &'static str,
    pr: &'static str,
    comment: &'static str,
    here: &'static str,
    open: &'static str,
    prune: &'static str,
    dirty: &'static str,
    needs: &'static str,
    done: &'static str,
    ci_ok: &'static str,
    ci_fail: &'static str,
    approved: &'static str,
    changes: &'static str,
    open_btn: &'static str,
    claude_btn: &'static str,
    hide_btn: &'static str,
    show_btn: &'static str,
}

fn icons() -> Icons {
    if std::env::var("WORKTREES_ICONS").as_deref() == Ok("plain") {
        Icons {
            repo: "⎇",
            pr: "#",
            comment: "c",
            here: "◆",
            open: "◇",
            prune: "✗",
            dirty: "✎",
            needs: "◉",
            done: "✓",
            ci_ok: "✓",
            ci_fail: "✗",
            approved: "✔",
            changes: "±",
            open_btn: "↗",
            claude_btn: "▶",
            hide_btn: "⊘",
            show_btn: "◉",
        }
    } else {
        Icons {
            repo: "\u{e0a0}",
            pr: "\u{f407} ",
            comment: "\u{f075} ",
            here: "\u{f041}",     // map marker: the checkout this tab is in
            open: "\u{eb7f}",     // window: open in another tab
            prune: "\u{f1f8}",    // trash: folder is gone, prunable
            dirty: "\u{f040} ",   // pencil: uncommitted files
            needs: "\u{f0f3} ",   // bell: an agent is waiting on you
            done: "\u{f11e} ",    // checkered flag: an agent finished
            ci_ok: "\u{f058} ",   // check circle
            ci_fail: "\u{f057} ", // x circle
            approved: "\u{f164}", // thumbs up
            changes: "\u{f165}",  // thumbs down: changes requested
            open_btn: "\u{f08e}",   // external link: go to its workspace
            claude_btn: "\u{f120}", // terminal: start claude there
            hide_btn: "\u{f070}",   // eye slash: off the board
            show_btn: "\u{f06e}",   // eye: back on the board
        }
    }
}

pub fn render(f: &mut Frame, app: &mut App) {
    let foot = footer(app, f.area().width);
    let [head, body, foot_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(1), Constraint::Length(foot.len() as u16)])
            .areas(f.area());
    header(f, app, head);
    if app.legend {
        app.hits.clear();
        f.render_widget(Paragraph::new(legend()), body);
    } else {
        list(f, app, body);
    }
    f.render_widget(Paragraph::new(foot), foot_area);
}

fn spread(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: u16) -> Line<'static> {
    let lw: usize = left.iter().map(Span::width).sum();
    let rw: usize = right.iter().map(Span::width).sum();
    let pad = (width as usize).saturating_sub(lw + rw).max(1);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(right);
    Line::from(spans)
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{keep}…")
}

fn rule(width: u16) -> Line<'static> {
    Line::from("─".repeat(width as usize)).fg(Color::DarkGray)
}

fn header(f: &mut Frame, app: &App, area: Rect) {
    let ic = icons();
    let w = area.width;
    let title = spread(
        vec![" ".into(), ic.repo.fg(Color::Cyan).bold(), " ".into(), app.repo.clone().bold()],
        vec![format!("vs {} ", app.base).fg(Color::DarkGray)],
        w,
    );
    let count = |pred: fn(&Row) -> bool| app.rows.iter().filter(|r| pred(r)).count();
    let (blocked, working, done, failed) = (
        count(|r| r.top_status() == "blocked"),
        count(|r| r.top_status() == "working"),
        count(|r| r.top_status() == "done"),
        count(Row::ci_failed),
    );
    let n = app.rows.len();
    let mut left: Vec<Span> = vec![format!(" {n} worktree{}", if n == 1 { "" } else { "s" }).fg(Color::DarkGray)];
    let hidden = app.snap.worktrees.iter().filter(|w| app.hidden.contains(&w.path)).count();
    if hidden > 0 && !app.show_hidden {
        left.push(format!(" +{hidden} hidden").fg(Color::DarkGray));
    }
    let mut badge = |n: usize, glyph: &str, color: Color| {
        if n > 0 {
            left.push(format!("  {glyph}{n}").fg(color).bold());
        }
    };
    badge(blocked, ic.needs, Color::Red);
    badge(working, SPIN[app.frame() % 10], Color::Yellow);
    badge(done, ic.done, Color::Green);
    badge(failed, ic.ci_fail, Color::Red);
    let sync = if app.pr_at == 0 { "⟳ –".to_string() } else { format!("⟳ {} ", age(app.pr_at)) };
    let summary = spread(left, vec![sync.fg(Color::DarkGray)], w);
    f.render_widget(Paragraph::new(vec![title, summary, rule(w)]), area);
}

fn age(ts: i64) -> String {
    let s = now() - ts;
    if ts == 0 {
        String::new()
    } else if s < 60 {
        "now".into()
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
    }
}

/// Heat by recency: fresh work stands out, stale checkouts fade.
fn age_color(ts: i64) -> Color {
    let s = now() - ts;
    if s < 3600 {
        Color::Green
    } else if s < 86400 {
        Color::Reset
    } else if s < 7 * 86400 {
        Color::Gray
    } else {
        Color::DarkGray
    }
}

/// The card's signal color: what, if anything, this worktree needs from you.
fn signal(app: &App, r: &Row) -> Style {
    let base = match r.top_status() {
        "blocked" => Color::Red,
        _ if r.ci_failed() => Color::Red,
        "working" => Color::Yellow,
        "done" => Color::Green,
        _ => Color::DarkGray,
    };
    if app.pulse_on(&r.path) {
        Style::new().fg(Color::LightRed).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(base)
    }
}

fn card(app: &App, r: &Row, w: u16, sel: bool, hover: bool) -> Vec<Line<'static>> {
    let ic = icons();
    let sig = signal(app, r);
    let bar = || Span::styled("▌", sig);
    let spin = SPIN[app.frame() % 10];
    let g = r.git.clone().unwrap_or_default();

    // Line 1: place, branch, dirty / ahead / behind.
    let mark = match r.place {
        Place::Here => ic.here.fg(Color::Cyan),
        Place::Open => ic.open.fg(Color::Cyan),
        Place::Prunable => ic.prune.fg(Color::Red),
        Place::Closed => " ".into(),
    };
    let mut right: Vec<Span> = Vec::new();
    if g.dirty > 0 {
        right.push(format!("{}{} ", ic.dirty, g.dirty).fg(Color::Yellow));
    }
    if g.ahead > 0 {
        right.push(format!("↑{}", g.ahead).fg(Color::Green));
    }
    if g.behind > 0 {
        let c = match g.behind {
            50.. => Color::Red,
            10.. => Color::Yellow,
            _ => Color::DarkGray,
        };
        right.push(format!("↓{}", g.behind).fg(c));
    }
    right.push(" ".into());
    let room = (w as usize).saturating_sub(4 + right.iter().map(Span::width).sum::<usize>());
    let is_hidden = app.hidden.contains(&r.path);
    if is_hidden {
        right.insert(0, "hidden ".fg(Color::DarkGray).italic());
    }
    let room = room.saturating_sub(if is_hidden { 7 } else { 0 });
    let mut name = Span::raw(cut(&r.branch, room));
    if is_hidden {
        name = name.fg(Color::DarkGray);
    }
    if sel {
        name = name.bold();
    }
    if hover {
        name = name.underlined();
    }
    let mut lines = vec![spread(vec![bar(), " ".into(), mark, " ".into(), name], right, w)];

    // Agent lines: one per agent, most urgent first, each naming which coding agent it is.
    // The first line also carries the last-commit age.
    let age_span = || format!("{} ", age(g.last_ct)).fg(age_color(g.last_ct));
    if r.agents.is_empty() {
        lines.push(spread(vec![bar(), "   ".into(), "· no agent".fg(Color::DarkGray)], vec![age_span()], w));
    }
    for (i, a) in r.agents.iter().take(MAX_AGENT_LINES).enumerate() {
        let right = if i == 0 { vec![age_span()] } else { Vec::new() };
        let rw: usize = right.iter().map(Span::width).sum();
        lines.push(spread(agent_line(a, spin, &ic, w as usize - rw, bar()), right, w));
    }
    if r.agents.len() > MAX_AGENT_LINES {
        let more = format!("+{} more", r.agents.len() - MAX_AGENT_LINES);
        lines.push(Line::from(vec![bar(), "   ".into(), more.fg(Color::DarkGray)]));
    }

    // Line 3: PR, CI, unresolved threads.
    let mut left = vec![bar(), "   ".into()];
    match &r.pr {
        Some(pr) => {
            left.push(format!("{}{}", ic.pr, pr.n).fg(Color::Magenta));
            left.push(" ".into());
            left.push(match pr.ci.as_str() {
                "SUCCESS" => ic.ci_ok.trim_end().fg(Color::Green),
                "FAILURE" | "ERROR" if !pr.fails.is_empty() => {
                    format!("{}{}", ic.ci_fail, pr.fails.len()).fg(Color::Red).bold()
                }
                "FAILURE" | "ERROR" => ic.ci_fail.trim_end().fg(Color::Red).bold(),
                "PENDING" | "EXPECTED" => format!("{spin}{}", pr.pending).fg(Color::Yellow),
                _ => "–".fg(Color::DarkGray),
            });
            if pr.t > 0 {
                left.push(format!(" {}{}", ic.comment, pr.t).fg(Color::Yellow));
            }
            match pr.review.as_str() {
                "APPROVED" => left.push(format!(" {}", ic.approved).fg(Color::Green)),
                "CHANGES_REQUESTED" => left.push(format!(" {}", ic.changes).fg(Color::Red)),
                _ => {}
            }
            if pr.draft {
                left.push(" draft".fg(Color::DarkGray));
            }
        }
        None => left.push("no PR".fg(Color::DarkGray)),
    }
    lines.push(Line::from(left));

    if sel && app.expanded {
        lines.extend(details(app, r, &g, w));
    }
    lines
}

/// Agents listed per card before collapsing the rest into "+N more".
const MAX_AGENT_LINES: usize = 3;

/// `⠧ claude · what it is doing`: state glyph, agent name in bold, then the pane title.
fn agent_line(
    a: &super::data::Agent,
    spin: &'static str,
    ic: &Icons,
    width: usize,
    bar: Span<'static>,
) -> Vec<Span<'static>> {
    let (glyph, state, style) = match a.status.as_str() {
        "blocked" => (ic.needs.trim_end(), "needs you", Style::new().fg(Color::Red).bold()),
        "working" => (spin, "", Style::new().fg(Color::Yellow)),
        "done" => (ic.done.trim_end(), "done", Style::new().fg(Color::Green)),
        "idle" => ("●", "", Style::new().fg(Color::Gray)),
        _ => ("○", "", Style::new().fg(Color::DarkGray)),
    };
    let name_style = if a.status == "blocked" { style } else { Style::new().bold() };
    let what = [state, a.title.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · ");
    // bar + 3 spaces + glyph + space + name + " · " + 1 cell of breathing room before the right column.
    let room = width.saturating_sub(4 + 2 + a.name.chars().count() + 3 + 1);
    let mut spans =
        vec![bar, "   ".into(), Span::styled(format!("{glyph} "), style), Span::styled(a.name.clone(), name_style)];
    if !what.is_empty() && room > 1 {
        spans.push(" · ".fg(Color::DarkGray));
        spans.push(Span::styled(cut(&what, room), style));
    }
    spans
}

/// `?`: what each glyph on a card means, drawn with the glyphs the board is using.
fn legend() -> Vec<Line<'static>> {
    let ic = icons();
    let row = |glyph: String, color: Color, text: &'static str| {
        Line::from(vec![format!("  {glyph:<5} ").fg(color), text.into()])
    };
    let head = |t: &'static str| Line::from(format!(" {t}").fg(Color::DarkGray).bold());
    vec![
        head("checkout"),
        row(ic.here.into(), Color::Cyan, "this tab is in it"),
        row(ic.open.into(), Color::Cyan, "open in another tab"),
        row(ic.prune.into(), Color::Red, "folder gone, prunable"),
        row(format!("{}3", ic.dirty), Color::Yellow, "3 uncommitted files"),
        row("↑2↓5".into(), Color::Green, "commits ahead / behind"),
        Line::from(""),
        head("agents"),
        row(ic.needs.trim_end().into(), Color::Red, "waiting on you"),
        row(SPIN[0].into(), Color::Yellow, "working"),
        row(ic.done.trim_end().into(), Color::Green, "finished"),
        row("●".into(), Color::Gray, "idle"),
        Line::from(""),
        head("pull request"),
        row(format!("{}12", ic.pr), Color::Magenta, "PR number"),
        row(ic.ci_ok.trim_end().into(), Color::Green, "CI passed"),
        row(format!("{}2", ic.ci_fail), Color::Red, "2 CI checks failed"),
        row(format!("{}3", ic.comment), Color::Yellow, "3 open review threads"),
        row(ic.approved.into(), Color::Green, "approved"),
        row(ic.changes.into(), Color::Red, "changes requested"),
        Line::from(""),
        head("card bar"),
        row("▌".into(), Color::Red, "needs you or CI failed"),
        row("▌".into(), Color::Yellow, "agent working"),
        row("▌".into(), Color::Green, "agent finished"),
    ]
}

fn details(app: &App, r: &Row, g: &super::data::GitInfo, w: u16) -> Vec<Line<'static>> {
    let room = (w as usize).saturating_sub(5);
    let pad = || Span::raw("    ");
    let mut out = vec![Line::from("")];
    if !g.subject.is_empty() {
        out.push(Line::from(vec![pad(), format!("“{}”", cut(&g.subject, room - 2)).italic().fg(Color::Gray)]));
    }
    if g.ahead > 0 {
        let total = (g.ins + g.del).max(1);
        let green = (10 * g.ins).div_ceil(total).min(10) as usize;
        out.push(Line::from(vec![
            pad(),
            "▇".repeat(green).fg(Color::Green),
            "▇".repeat(10 - green).fg(Color::Red),
            format!(" +{} −{} · {}f", g.ins, g.del, g.files).fg(Color::DarkGray),
        ]));
    } else {
        out.push(Line::from(vec![pad(), format!("no commits ahead of {}", app.base).fg(Color::DarkGray)]));
    }
    if let Some(pr) = &r.pr {
        if !pr.fails.is_empty() {
            out.push(Line::from(vec![pad(), format!("✗ {}", cut(&pr.fails.join(", "), room - 2)).fg(Color::Red)]));
        }
    }
    let shown = r.path.strip_prefix(&app.root).map(|p| format!(".{p}")).unwrap_or_else(|| r.path.clone());
    out.push(Line::from(vec![pad(), cut(&shown, room).fg(Color::DarkGray)]));
    out
}

fn list(f: &mut Frame, app: &mut App, area: Rect) {
    app.hits.clear();
    app.act_hits.clear();
    if app.rows.is_empty() {
        let msg = match (&app.snap.error, app.filter.is_empty()) {
            (Some(e), _) => e.clone(),
            (None, false) => format!("no worktree matches “{}”", app.filter),
            (None, true) => "loading…".into(),
        };
        f.render_widget(
            Paragraph::new(msg).fg(Color::DarkGray).centered(),
            Rect { y: area.y + 1, height: area.height.saturating_sub(1), ..area },
        );
        return;
    }
    let w = area.width;
    let rows = app.rows.clone();
    let hover = app.hover.clone();
    // Target layout: selected card is framed (2 extra lines), others are separated by a gap.
    let mut cards = Vec::new();
    let mut y = 0u16;
    for r in &rows {
        let sel = app.selected.as_deref() == Some(r.path.as_str());
        let inner = if sel { w.saturating_sub(2) } else { w };
        let mut lines = card(app, r, inner, sel, hover.as_deref() == Some(r.path.as_str()));
        let mut buttons = Vec::new();
        if sel {
            let (line, b) = action_row(app, r, inner);
            lines.push(line);
            buttons = b;
        }
        let h = lines.len() as u16 + if sel { 2 } else { 1 };
        let bar_at = lines.len() as u16; // the action row's line inside the frame (1 = first)
        cards.push((r.path.clone(), sel, lines, y, h, buttons, bar_at));
        y += h;
    }
    // Keep the selected card fully visible.
    if let Some((_, _, _, sy, sh, _, _)) = cards.iter().find(|c| c.1) {
        if *sy < app.scroll {
            app.scroll = *sy;
        } else if sy + sh > app.scroll + area.height {
            app.scroll = (sy + sh).saturating_sub(area.height);
        }
    }
    app.scroll = app.scroll.min(y.saturating_sub(area.height));

    // Draw in order of animated position so a card sliding over another stays on top.
    let mut placed: Vec<_> = cards
        .into_iter()
        .map(|(path, sel, lines, ty, h, buttons, bar_at)| {
            let dy = app.slide_y(&path, ty as f32);
            (path, sel, lines, dy, h, buttons, bar_at)
        })
        .collect();
    placed.sort_by(|a, b| a.3.total_cmp(&b.3));
    for (path, sel, lines, dy, h, buttons, bar_at) in placed {
        let top = area.y as i32 + dy.round() as i32 - app.scroll as i32;
        let bottom = top + h as i32;
        let (vis_top, vis_bottom) = (top.max(area.y as i32), bottom.min(area.bottom() as i32));
        if vis_top >= vis_bottom {
            continue;
        }
        let rect = Rect::new(area.x, vis_top as u16, w, (vis_bottom - vis_top) as u16);
        let skip = (vis_top - top) as u16;
        f.render_widget(Clear, rect);
        let row = app.rows.iter().find(|r| r.path == path).cloned();
        let para = Paragraph::new(lines).scroll((skip, 0));
        if sel {
            let border = row.as_ref().map(|r| signal(app, r)).unwrap_or_default();
            let border = if border.fg == Some(Color::DarkGray) { Style::new().fg(Color::Cyan) } else { border };
            let block = Block::new().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(border);
            f.render_widget(para.block(block), rect);
        } else {
            f.render_widget(para, rect);
        }
        // Buttons sit on the frame's last inner line; record them only when that line is visible.
        let bar_y = top + bar_at as i32;
        if sel && bar_y >= vis_top && bar_y < vis_bottom {
            for (x, width, a) in buttons {
                app.act_hits.push((Rect::new(area.x + 1 + x, bar_y as u16, width, 1), a));
            }
        }
        app.hits.push((rect, path));
    }
}

/// Clickable buttons on the selected card: icon + word, or icons alone when the card is narrow.
/// Returns the line and each button's (x, width) inside the card.
fn action_row(app: &App, r: &Row, w: u16) -> (Line<'static>, Vec<(u16, u16, Act)>) {
    let ic = icons();
    let mut acts = vec![(ic.open_btn, "open", Act::Open, Color::Cyan), (ic.claude_btn, "claude", Act::Claude, Color::Yellow)];
    if r.pr.as_ref().is_some_and(|p| !p.url.is_empty()) {
        acts.push((ic.pr, "PR", Act::Pr, Color::Magenta));
    }
    let hidden = app.hidden.contains(&r.path);
    acts.push(if hidden { (ic.show_btn, "unhide", Act::Hide, Color::Gray) } else { (ic.hide_btn, "hide", Act::Hide, Color::Gray) });
    if r.linked {
        acts.push((ic.prune, "del", Act::Delete, Color::Red));
    }
    let label = |g: &str, word: &str, words: bool| {
        let g = g.trim_end();
        if words { format!("{g} {word}") } else { g.to_string() }
    };
    let lead = 3u16;
    let full: usize = acts.iter().map(|(g, wd, ..)| label(g, wd, true).chars().count() + 2).sum();
    let words = full + lead as usize <= w as usize;
    let mut spans: Vec<Span<'static>> = vec![Span::raw("   ")];
    let mut hits = Vec::new();
    let mut x = lead;
    for (g, wd, a, color) in acts {
        let text = label(g, wd, words);
        let width = text.chars().count() as u16 + 1;
        spans.push(Span::styled(text, Style::new().fg(color)));
        spans.push(Span::raw("  "));
        hits.push((x, width, a));
        x += width + 1;
    }
    (Line::from(spans), hits)
}

fn hints(pairs: &[(&str, &str)], width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![Vec::<Span>::new()];
    let mut used = 0usize;
    for (k, label) in pairs {
        let len = k.chars().count() + label.chars().count() + 3;
        if used + len > width as usize && used > 0 {
            lines.push(Vec::new());
            used = 0;
        }
        let cur = lines.last_mut().expect("non-empty");
        cur.push(format!(" {k}").fg(Color::Cyan));
        cur.push(format!(" {label} ").fg(Color::DarkGray));
        used += len;
    }
    lines.into_iter().map(Line::from).collect()
}

fn footer(app: &App, w: u16) -> Vec<Line<'static>> {
    let cursor = || "▏".fg(Color::Cyan);
    let (prompt, keys): (Line, Vec<(&str, &str)>) = match &app.mode {
        Mode::Filter => (
            Line::from(vec![" / ".fg(Color::Cyan), app.filter.clone().into(), cursor()]),
            vec![("↵", "keep"), ("esc", "clear")],
        ),
        Mode::NewBranch(s) => (
            Line::from(vec![" new branch ".fg(Color::Cyan), s.clone().into(), cursor()]),
            vec![("↵", "create"), ("esc", "cancel")],
        ),
        Mode::ConfirmDelete(p) => {
            let r = app.rows.iter().find(|r| &r.path == p);
            let b = r.map(|r| r.branch.clone()).unwrap_or_default();
            let dirty = r.and_then(|r| r.git.as_ref()).map_or(0, |g| g.dirty);
            if dirty > 0 {
                (
                    Line::from(format!(" {dirty} uncommitted file{} in {} will be lost", if dirty == 1 { "" } else { "s" }, cut(&b, (w as usize).saturating_sub(36))))
                        .fg(Color::Red)
                        .bold(),
                    vec![("f", "remove anyway"), ("n", "no")],
                )
            } else {
                (
                    Line::from(format!(" remove {}? ", cut(&b, (w as usize).saturating_sub(14)))).fg(Color::Red).bold(),
                    vec![("y", "worktree"), ("b", "+ branch"), ("n", "no")],
                )
            }
        }
        Mode::Normal if app.legend => (Line::from(""), vec![("?", "close"), ("esc", "close")]),
        Mode::Normal => {
            let status = app.status.as_ref().filter(|(_, _, at)| at.elapsed().as_secs() < 5);
            let prompt = match status {
                Some((t, ok, _)) => {
                    Line::from(format!(" {}", cut(t, w as usize - 2))).fg(if *ok { Color::Green } else { Color::Red })
                }
                None if !app.filter.is_empty() => Line::from(vec![" / ".fg(Color::Cyan), app.filter.clone().into()]),
                None => Line::from(""),
            };
            (
                prompt,
                vec![
                    ("↵", "open"),
                    ("⇥", "info"),
                    ("n", "new"),
                    ("d", "del"),
                    ("x", "hide"),
                    ("c", "claude"),
                    ("o", "PR"),
                    ("y", "copy"),
                    ("/", "find"),
                    ("?", "icons"),
                    ("H", "show hidden"),
                    ("q", "close"),
                    ("Q", "close all"),
                ],
            )
        }
    };
    let mut lines = vec![rule(w), prompt];
    lines.extend(hints(&keys, w));
    lines
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Mutex};

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::worktrees::app::App;
    use crate::worktrees::data::{Agent, HerdrSnap, Msg, WtRaw};

    fn render(agents: Vec<(&str, &str, &str)>) -> Vec<String> {
        let (tx, _rx) = channel();
        let kicks = (0..3).map(|_| channel().0).collect();
        let mut app = App::new(
            "w1".into(),
            String::new(),
            "repo".into(),
            "/repo".into(),
            "origin/main".into(),
            String::new(),
            Arc::new(Mutex::new(Vec::new())),
            tx,
            kicks,
        );
        let wt = |path: &str, branch: &str| WtRaw {
            path: path.into(),
            branch: branch.into(),
            prunable: false,
            linked: path != "/repo",
            open_ws: None,
        };
        app.apply(Msg::Herdr(HerdrSnap {
            worktrees: vec![wt("/repo", "main"), wt("/repo/.worktrees/feat", "feat")],
            agents: agents
                .into_iter()
                .map(|(name, status, title)| {
                    let a = Agent { status: status.into(), name: name.into(), title: title.into() };
                    ("/repo/.worktrees/feat".to_string(), a)
                })
                .collect(),
            error: None,
        }));
        let Ok(mut term) = Terminal::new(TestBackend::new(38, 30));
        let Ok(_) = term.draw(|f| super::render(f, &mut app));
        let buf = term.backend().buffer().clone();
        (0..30).map(|y| (0..38).map(|x| buf[(x, y)].symbol().to_string()).collect()).collect()
    }

    #[test]
    fn every_agent_in_a_worktree_is_named_most_urgent_first() {
        let lines = render(vec![("claude", "working", "Fix login"), ("codex", "blocked", "Review")]);
        let at = |needle: &str| lines.iter().position(|l| l.contains(needle));
        let (codex, claude) = (at("codex").expect("codex shown"), at("claude").expect("claude shown"));
        assert!(codex < claude, "the agent waiting on you is listed first");
        assert!(lines[codex].contains("needs you"));
        assert!(lines[claude].contains("Fix login"));
    }

    #[test]
    fn clicking_the_selected_cards_buttons_hides_it_and_asks_before_deleting() {
        use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        use crate::worktrees::app::{Act, Mode};

        let (tx, _rx) = channel();
        let kicks = (0..3).map(|_| channel().0).collect();
        let mut app = App::new("w1".into(), String::new(), "repo".into(), "/repo".into(), "origin/main".into(), String::new(), Arc::new(Mutex::new(Vec::new())), tx, kicks);
        let wt = |path: &str, branch: &str| WtRaw { path: path.into(), branch: branch.into(), prunable: false, linked: path != "/repo", open_ws: None };
        app.apply(Msg::Herdr(HerdrSnap { worktrees: vec![wt("/repo", "main"), wt("/repo/.worktrees/feat", "feat")], agents: Vec::new(), error: None }));
        app.selected = Some("/repo/.worktrees/feat".into());
        let Ok(mut term) = Terminal::new(TestBackend::new(38, 30));
        let mut draw = |app: &mut App| {
            let Ok(_) = term.draw(|f| super::render(f, app));
        };
        draw(&mut app);
        let click = |app: &mut App, a: Act| {
            let (r, _) = *app.act_hits.iter().find(|(_, x)| *x == a).expect("button drawn");
            let m = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x, row: r.y, modifiers: KeyModifiers::NONE };
            crate::worktrees::mouse(app, m);
        };

        click(&mut app, Act::Delete);
        assert!(matches!(app.mode, Mode::ConfirmDelete(ref p) if p == "/repo/.worktrees/feat"), "del only asks");

        app.mode = Mode::Normal;
        click(&mut app, Act::Hide);
        assert!(app.rows.iter().all(|r| r.branch != "feat"), "hide takes the card off the board");
        app.selected = Some("/repo".into());
        draw(&mut app);
        assert!(app.act_hits.iter().all(|(_, a)| *a != Act::Delete), "the main checkout has no delete button");
    }

    #[test]
    fn legend_explains_every_card_glyph_within_the_panel_width() {
        let lines: Vec<String> = super::legend().iter().map(|l| l.to_string()).collect();
        for meaning in ["this tab is in it", "uncommitted", "waiting on you", "CI checks failed", "approved"] {
            assert!(lines.iter().any(|l| l.contains(meaning)), "legend explains {meaning}");
        }
        let widest = super::legend().iter().map(|l| l.width()).max().unwrap_or(0);
        assert!(widest <= 38, "legend fits the default 38-column board, widest = {widest}");
    }

    #[test]
    fn agents_beyond_the_limit_collapse_into_a_count() {
        let lines =
            render(vec![("claude", "idle", ""), ("codex", "idle", ""), ("pi", "idle", ""), ("opencode", "idle", "")]);
        assert!(lines.iter().any(|l| l.contains("+1 more")));
        assert!(!lines.iter().any(|l| l.contains("opencode")));
    }
}
