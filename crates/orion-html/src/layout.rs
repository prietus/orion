//! Layout at a fixed width in terminal cells.

use std::cell::Cell;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::build::{El, Kind, Node};
use crate::css::{Align, Color};
use crate::{Doc, Placement, Rendered, Span, StyledLine, TextStyle};

pub struct LayoutOptions<'a> {
    /// Size of one terminal cell in pixels (width, height).
    pub cell: (u16, u16),
    /// Maximum height of an image, in lines.
    pub max_image_rows: u16,
    /// Appends `[n]` after each link.
    pub link_markers: bool,
    /// Pixel dimensions of image `i` in [`Doc::images`] if it is available
    /// locally (attached via `cid:`); `None` = it cannot be shown.
    pub image_px: &'a dyn Fn(usize) -> Option<(u32, u32)>,
}

impl Default for LayoutOptions<'_> {
    fn default() -> Self {
        LayoutOptions { cell: (10, 20), max_image_rows: 40, link_markers: true, image_px: &|_| None }
    }
}

struct Ctx<'a, 'o> {
    doc: &'a Doc,
    opts: &'a LayoutOptions<'o>,
    list_depth: Cell<usize>,
    quote_depth: Cell<usize>,
}

pub(crate) fn layout(doc: &Doc, width: u16, opts: &LayoutOptions<'_>) -> Rendered {
    let ctx = Ctx { doc, opts, list_depth: Cell::new(0), quote_depth: Cell::new(0) };
    let mut out = Out::default();
    layout_el(&ctx, &doc.root, width, &mut out);
    // No trailing blank lines.
    while out.lines.last().is_some_and(|l| l.text().trim().is_empty()) && out.placements.iter().all(|p| p.line + (p.height as usize) < out.lines.len()) {
        out.lines.pop();
    }
    Rendered { lines: out.lines, placements: out.placements }
}

// ------------------------------------------------------------------ output

#[derive(Default)]
struct Out {
    lines: Vec<StyledLine>,
    placements: Vec<Placement>,
    /// Pending margin: becomes a blank line if more content follows.
    want_gap: bool,
}

impl Out {
    fn gap(&mut self) {
        self.want_gap = true;
    }

    fn apply_gap(&mut self) {
        if self.want_gap && self.lines.last().is_some_and(|l| !l.text().trim().is_empty()) {
            self.lines.push(StyledLine::default());
        }
        self.want_gap = false;
    }

    fn push(&mut self, line: StyledLine) {
        self.apply_gap();
        self.lines.push(line);
    }

    /// Appends an already laid-out block with prefixes (indent, bullet, quote bar).
    fn append(&mut self, child: Out, first: &[Span], rest: &[Span]) {
        if child.lines.is_empty() {
            self.want_gap |= child.want_gap;
            return;
        }
        self.apply_gap();
        let base = self.lines.len();
        let indent = spans_width(first) as u16;
        for (i, line) in child.lines.into_iter().enumerate() {
            let mut spans = if i == 0 { first.to_vec() } else { rest.to_vec() };
            spans.extend(line.spans);
            self.lines.push(StyledLine { spans });
        }
        self.placements.extend(child.placements.into_iter().map(|p| Placement { line: p.line + base, x: p.x + indent, ..p }));
        self.want_gap = child.want_gap;
    }
}

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.text.width()).sum()
}

fn plain(text: impl Into<String>) -> Span {
    Span { text: text.into(), style: TextStyle::default() }
}

fn dim(text: impl Into<String>, fg: Option<Color>) -> Span {
    Span { text: text.into(), style: TextStyle { dim: fg.is_none(), fg, ..Default::default() } }
}

// ----------------------------------------------------------------- blocks

fn layout_el(ctx: &Ctx, el: &El, width: u16, out: &mut Out) {
    match el.kind {
        Kind::Block { margin } => {
            if margin {
                out.gap();
            }
            layout_children(ctx, el, width, out);
            if margin {
                out.gap();
            }
        }
        Kind::Heading => {
            out.gap();
            layout_children(ctx, el, width, out);
            out.gap();
        }
        Kind::Hr => {
            out.gap();
            out.push(StyledLine { spans: vec![dim("─".repeat(width as usize), None)] });
            out.gap();
        }
        Kind::Quote => {
            let depth = ctx.quote_depth.get();
            ctx.quote_depth.set(depth + 1);
            let mut inner = Out::default();
            layout_children(ctx, el, width.saturating_sub(2).max(1), &mut inner);
            ctx.quote_depth.set(depth);
            let bar = [dim("│ ", Some(Color::Ansi([2, 4, 5, 3][depth % 4])))];
            out.gap();
            out.append(inner, &bar, &bar);
            out.gap();
        }
        Kind::List { ordered, start } => {
            let depth = ctx.list_depth.get();
            ctx.list_depth.set(depth + 1);
            if depth == 0 {
                out.gap();
            }
            let items = el.children.iter().filter(|c| matches!(c, Node::El(e) if e.kind == Kind::Item)).count() as u32;
            let num_w = if ordered { format!("{}.", start + items.saturating_sub(1)).len() + 1 } else { 2 };
            let mut n = start;
            let mut run = Vec::new();
            for child in &el.children {
                match child {
                    Node::El(item) if item.kind == Kind::Item => {
                        flush_inline(ctx, &mut run, el, width, out);
                        let marker = if ordered {
                            format!("{:>w$} ", format!("{n}."), w = num_w - 1)
                        } else {
                            format!("{} ", ["•", "◦", "▪"][depth % 3])
                        };
                        n += 1;
                        let mut inner = Out::default();
                        layout_children(ctx, item, width.saturating_sub(num_w as u16).max(1), &mut inner);
                        if inner.lines.is_empty() {
                            inner.lines.push(StyledLine::default());
                        }
                        out.append(inner, &[dim(marker, None)], &[plain(" ".repeat(num_w))]);
                    }
                    other => run.push(other),
                }
            }
            flush_inline(ctx, &mut run, el, width, out);
            ctx.list_depth.set(depth);
            if depth == 0 {
                out.gap();
            }
        }
        Kind::Table { border } => layout_table(ctx, el, border, width, out),
        // Stray elements outside their context: treated as ordinary blocks.
        Kind::Item | Kind::RowGroup | Kind::Row | Kind::Cell { .. } | Kind::Inline | Kind::Link(_) => {
            layout_children(ctx, el, width, out)
        }
        Kind::Br | Kind::Img(_) => {
            let mut run = vec![];
            let node = Node::El(el.clone());
            run.push(&node);
            flush_inline(ctx, &mut run, el, width, out);
        }
    }
}

/// Lays out the children: inline nodes are grouped into paragraphs, blocks go on their own.
fn layout_children(ctx: &Ctx, el: &El, width: u16, out: &mut Out) {
    let mut run: Vec<&Node> = Vec::new();
    for child in &el.children {
        match child {
            Node::El(c) if c.block => {
                flush_inline(ctx, &mut run, el, width, out);
                layout_el(ctx, c, width, out);
            }
            other => run.push(other),
        }
    }
    flush_inline(ctx, &mut run, el, width, out);
}

// ---------------------------------------------------------------- inline

enum Item {
    Text { text: String, style: TextStyle, pre: bool },
    Break,
    Image(usize),
}

fn flush_inline(ctx: &Ctx, run: &mut Vec<&Node>, parent: &El, width: u16, out: &mut Out) {
    if run.is_empty() {
        return;
    }
    let mut items = Vec::new();
    for node in run.drain(..) {
        collect(ctx, node, &parent.style, parent.pre, &mut items);
    }
    wrap(ctx, &items, width, parent.align, out);
}

fn collect(ctx: &Ctx, node: &Node, style: &TextStyle, pre: bool, items: &mut Vec<Item>) {
    match node {
        Node::Text(t) => {
            if pre {
                items.push(Item::Text { text: t.replace('\t', "    ").replace('\r', ""), style: style.clone(), pre: true });
            } else {
                // Collapse whitespace (&nbsp; arrives as U+00A0 and is not collapsed).
                let mut s = String::with_capacity(t.len());
                let mut space = false;
                for c in t.chars() {
                    if c.is_whitespace() && c != '\u{a0}' {
                        space = true;
                    } else {
                        if space {
                            s.push(' ');
                            space = false;
                        }
                        s.push(c);
                    }
                }
                if space {
                    s.push(' ');
                }
                if !s.is_empty() {
                    items.push(Item::Text { text: s, style: style.clone(), pre: false });
                }
            }
        }
        Node::El(el) => match el.kind {
            Kind::Br => items.push(Item::Break),
            Kind::Img(i) => {
                if (ctx.opts.image_px)(i).is_some() {
                    items.push(Item::Image(i));
                } else {
                    let alt = &ctx.doc.images[i].alt;
                    if !alt.is_empty() {
                        let st = TextStyle { dim: true, italic: true, link: el.style.link, ..Default::default() };
                        items.push(Item::Text { text: format!("[{alt}]"), style: st, pre: false });
                    }
                }
            }
            _ => {
                for c in &el.children {
                    collect(ctx, c, &el.style, el.pre, items);
                }
                if let Kind::Link(i) = el.kind
                    && ctx.opts.link_markers {
                        let st = TextStyle { dim: true, ..Default::default() };
                        items.push(Item::Text { text: format!("[{}]", i + 1), style: st, pre: false });
                    }
            }
        },
    }
}

/// Greedy word wrapping, with styles and alignment.
fn wrap(ctx: &Ctx, items: &[Item], width: u16, align: Align, out: &mut Out) {
    let width = width as usize;
    let mut line: Vec<Span> = Vec::new();
    let mut line_w = 0usize;
    let mut pending_space: Option<TextStyle> = None;

    fn push_span(line: &mut Vec<Span>, text: &str, style: &TextStyle) {
        // A hard space (&nbsp;) keeps words together when wrapping, but is drawn as a space.
        let text = text.replace('\u{a0}', " ");
        match line.last_mut() {
            Some(last) if last.style == *style => last.text.push_str(&text),
            _ => line.push(Span { text, style: style.clone() }),
        }
    }

    let flush = |line: &mut Vec<Span>, line_w: &mut usize, out: &mut Out, force: bool| {
        if line.is_empty() && !force {
            return;
        }
        if let Some(last) = line.last_mut() {
            let trimmed = last.text.trim_end_matches(' ').len();
            last.text.truncate(trimmed);
        }
        let w = spans_width(line);
        let pad = match align {
            Align::Left => 0,
            Align::Center => width.saturating_sub(w) / 2,
            Align::Right => width.saturating_sub(w),
        };
        let mut spans = Vec::with_capacity(line.len() + 1);
        if pad > 0 && w > 0 {
            spans.push(plain(" ".repeat(pad)));
        }
        spans.append(line);
        out.push(StyledLine { spans });
        *line_w = 0;
    };

    for item in items {
        match item {
            Item::Break => {
                flush(&mut line, &mut line_w, out, true);
                pending_space = None;
            }
            Item::Image(i) => {
                flush(&mut line, &mut line_w, out, false);
                pending_space = None;
                place_image(ctx, *i, width as u16, align, out);
            }
            Item::Text { text, style, pre } => {
                for token in tokens(text) {
                    let is_space = token.starts_with(' ');
                    if is_space && !pre {
                        if line_w > 0 {
                            pending_space = Some(style.clone());
                        }
                        continue;
                    }
                    let tw = token.width();
                    let space_w = usize::from(pending_space.is_some());
                    if line_w > 0 && line_w + space_w + tw > width {
                        flush(&mut line, &mut line_w, out, false);
                        pending_space = None;
                        if is_space {
                            continue; // spaces don't start a wrapped line
                        }
                    }
                    if let Some(st) = pending_space.take() {
                        push_span(&mut line, " ", &st);
                        line_w += 1;
                    }
                    if tw <= width.saturating_sub(line_w) {
                        push_span(&mut line, token, style);
                        line_w += tw;
                    } else {
                        // Word longer than the line: split by characters.
                        for ch in token.chars() {
                            let cw = ch.width().unwrap_or(0);
                            if line_w + cw > width && line_w > 0 {
                                flush(&mut line, &mut line_w, out, false);
                            }
                            push_span(&mut line, ch.encode_utf8(&mut [0; 4]), style);
                            line_w += cw;
                        }
                    }
                    }
            }
        }
    }
    flush(&mut line, &mut line_w, out, false);
}

/// Splits into words and runs of spaces, keeping both.
fn tokens(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let space = rest.starts_with(' ');
        let end = rest.find(|c: char| (c == ' ') != space).unwrap_or(rest.len());
        let (tok, r) = rest.split_at(end);
        rest = r;
        Some(tok)
    })
}

fn image_cells(ctx: &Ctx, i: usize, width: u16) -> Option<(u16, u16)> {
    let (pw, ph) = (ctx.opts.image_px)(i)?;
    let r = &ctx.doc.images[i];
    // The HTML width/height attributes rescale the image.
    let (w, h) = match (r.width, r.height) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, (ph as u64 * w as u64 / pw.max(1) as u64) as u32),
        (None, Some(h)) => ((pw as u64 * h as u64 / ph.max(1) as u64) as u32, h),
        (None, None) => (pw, ph),
    };
    if w <= 2 || h <= 2 {
        return None;
    }
    let (cw, ch) = (ctx.opts.cell.0.max(1) as f64, ctx.opts.cell.1.max(1) as f64);
    let mut cols = (w as f64 / cw).ceil();
    let mut rows = (h as f64 / ch).ceil();
    if cols > width as f64 {
        rows *= width as f64 / cols;
        cols = width as f64;
    }
    let max_rows = ctx.opts.max_image_rows.max(1) as f64;
    if rows > max_rows {
        cols *= max_rows / rows;
        rows = max_rows;
    }
    Some((cols.round().max(1.0) as u16, rows.round().max(1.0) as u16))
}

fn place_image(ctx: &Ctx, i: usize, width: u16, align: Align, out: &mut Out) {
    let Some((w, h)) = image_cells(ctx, i, width) else { return };
    out.apply_gap();
    let x = match align {
        Align::Left => 0,
        Align::Center => (width - w) / 2,
        Align::Right => width - w,
    };
    out.placements.push(Placement { line: out.lines.len(), x, width: w, height: h, image: i });
    for _ in 0..h {
        out.lines.push(StyledLine::default());
    }
}

// ---------------------------------------------------------------- measuring

/// (minimum width, natural width) of a list of nodes.
fn measure_children(ctx: &Ctx, el: &El) -> (u16, u16) {
    let (mut min, mut max) = (0u16, 0u16);
    let mut run: Vec<&Node> = Vec::new();
    let mut add = |(a, b): (u16, u16)| {
        min = min.max(a);
        max = max.max(b);
    };
    for child in &el.children {
        match child {
            Node::El(c) if c.block => {
                add(measure_inline(ctx, &mut run, el));
                add(measure_el(ctx, c));
            }
            other => run.push(other),
        }
    }
    add(measure_inline(ctx, &mut run, el));
    (min, max)
}

fn measure_inline(ctx: &Ctx, run: &mut Vec<&Node>, parent: &El) -> (u16, u16) {
    let mut items = Vec::new();
    for node in run.drain(..) {
        collect(ctx, node, &parent.style, parent.pre, &mut items);
    }
    let (mut min, mut max, mut cur) = (0usize, 0usize, 0usize);
    for item in &items {
        match item {
            Item::Break => {
                max = max.max(cur);
                cur = 0;
            }
            Item::Image(i) => {
                let w = image_cells(ctx, *i, u16::MAX).map(|(w, _)| w as usize).unwrap_or(0);
                min = min.max(w.min(40));
                max = max.max(cur).max(w);
                cur = 0;
            }
            Item::Text { text, pre, .. } => {
                for t in tokens(text) {
                    if !t.starts_with(' ') || *pre {
                        min = min.max(t.width());
                    }
                }
                cur += text.width();
            }
        }
    }
    max = max.max(cur);
    // A very long word (URL) must not widen a column without limit.
    (min.min(30) as u16, max.min(4000) as u16)
}

fn measure_el(ctx: &Ctx, el: &El) -> (u16, u16) {
    match el.kind {
        Kind::Table { border } => {
            let t = TableModel::new(el);
            let (mins, maxs) = t.measure(ctx);
            let gaps = t.gap_total(border, &maxs) as u16;
            (mins.iter().sum::<u16>() + gaps, maxs.iter().fold(0u16, |a, &b| a.saturating_add(b)).saturating_add(gaps))
        }
        Kind::Quote => {
            let (a, b) = measure_children(ctx, el);
            (a + 2, b.saturating_add(2))
        }
        Kind::List { .. } => {
            let (a, b) = measure_children(ctx, el);
            (a + 3, b.saturating_add(3))
        }
        Kind::Hr => (1, 1),
        Kind::Br => (0, 0),
        Kind::Img(_) => {
            let node = Node::El(el.clone());
            measure_inline(ctx, &mut vec![&node], el)
        }
        _ => measure_children(ctx, el),
    }
}

// ----------------------------------------------------------------- tables

struct TableModel<'a> {
    caption: Vec<&'a El>,
    rows: Vec<Vec<(&'a El, u16)>>,
    ncols: usize,
}

impl<'a> TableModel<'a> {
    fn new(table: &'a El) -> Self {
        let mut rows = Vec::new();
        let mut caption = Vec::new();
        fn gather<'a>(el: &'a El, rows: &mut Vec<Vec<(&'a El, u16)>>, caption: &mut Vec<&'a El>) {
            for c in &el.children {
                let Node::El(c) = c else { continue };
                match c.kind {
                    Kind::RowGroup => gather(c, rows, caption),
                    Kind::Row => rows.push(
                        c.children
                            .iter()
                            .filter_map(|n| match n {
                                Node::El(cell) => match cell.kind {
                                    Kind::Cell { colspan } => Some((cell, colspan)),
                                    _ => None,
                                },
                                _ => None,
                            })
                            .collect(),
                    ),
                    Kind::Block { .. } => caption.push(c),
                    _ => {}
                }
            }
        }
        gather(table, &mut rows, &mut caption);
        rows.retain(|r: &Vec<(&El, u16)>| !r.is_empty());
        let ncols = rows.iter().map(|r| r.iter().map(|(_, s)| *s as usize).sum::<usize>()).max().unwrap_or(0);
        TableModel { caption, rows, ncols }
    }

    fn measure(&self, ctx: &Ctx) -> (Vec<u16>, Vec<u16>) {
        let mut mins = vec![0u16; self.ncols];
        let mut maxs = vec![0u16; self.ncols];
        let mut spanned = Vec::new();
        for row in &self.rows {
            let mut col = 0;
            for &(cell, span) in row {
                let m = measure_children(ctx, cell);
                if span == 1 {
                    mins[col] = mins[col].max(m.0);
                    maxs[col] = maxs[col].max(m.1);
                } else {
                    spanned.push((col, span as usize, m));
                }
                col += span as usize;
            }
        }
        // Spanning cells: spread whatever is missing across their columns.
        for (col, span, (mn, mx)) in spanned {
            let end = (col + span).min(self.ncols);
            let cols = end - col;
            if cols == 0 {
                continue;
            }
            let have_min: u16 = mins[col..end].iter().sum();
            if mn > have_min {
                let extra = (mn - have_min).div_ceil(cols as u16);
                mins[col..end].iter_mut().for_each(|m| *m += extra);
            }
            let have_max: u16 = maxs[col..end].iter().fold(0u16, |a, &b| a.saturating_add(b));
            if mx > have_max {
                let extra = (mx - have_max).div_ceil(cols as u16);
                maxs[col..end].iter_mut().for_each(|m| *m = m.saturating_add(extra));
            }
        }
        for (mn, mx) in mins.iter_mut().zip(maxs.iter_mut()) {
            *mx = (*mx).max(*mn);
        }
        (mins, maxs)
    }

    /// Empty columns (spacers) get no gap.
    fn gap_total(&self, border: bool, maxs: &[u16]) -> usize {
        let visible = maxs.iter().filter(|&&m| m > 0).count();
        visible.saturating_sub(1) * if border { 3 } else { 2 }
    }
}

fn layout_table(ctx: &Ctx, el: &El, border: bool, width: u16, out: &mut Out) {
    let t = TableModel::new(el);
    for c in &t.caption {
        layout_el(ctx, c, width, out);
    }
    if t.ncols <= 1 {
        // Single-column layout table: cells are stacked blocks.
        for row in &t.rows {
            for (cell, _) in row {
                layout_children(ctx, cell, width, out);
            }
        }
        return;
    }
    let (mins, maxs) = t.measure(ctx);
    let gap = if border { 3 } else { 2 };
    let avail = (width as usize).saturating_sub(t.gap_total(border, &maxs));
    let widths = distribute(&mins, &maxs, avail);

    for row in &t.rows {
        // Lay out each cell at its width (sum of the columns it spans).
        let mut cells = Vec::new();
        let mut col = 0;
        for &(cell, span) in row {
            let end = (col + span as usize).min(t.ncols);
            let vis: Vec<usize> = (col..end).filter(|&c| widths[c] > 0).collect();
            let w = vis.iter().map(|&c| widths[c]).sum::<usize>() + vis.len().saturating_sub(1) * gap;
            let mut inner = Out::default();
            if w > 0 {
                layout_children(ctx, cell, w as u16, &mut inner);
            }
            while inner.lines.last().is_some_and(|l| l.text().trim().is_empty())
                && inner.placements.iter().all(|p| p.line + (p.height as usize) < inner.lines.len())
            {
                inner.lines.pop();
            }
            cells.push((inner, w));
            col = end;
        }
        let height = cells.iter().map(|(o, _)| o.lines.len()).max().unwrap_or(0);
        if height == 0 {
            continue;
        }
        out.apply_gap();
        let base = out.lines.len();
        let header = row.iter().all(|(c, _)| c.style.bold);
        for i in 0..height {
            let mut spans = Vec::new();
            let mut first = true;
            for (inner, w) in &cells {
                if *w == 0 {
                    continue;
                }
                if !first {
                    spans.push(if border { dim(" │ ", None) } else { plain("  ") });
                }
                first = false;
                let line = inner.lines.get(i).cloned().unwrap_or_default();
                let lw = line.width();
                spans.extend(line.spans);
                spans.push(plain(" ".repeat(w.saturating_sub(lw))));
            }
            out.lines.push(StyledLine { spans });
        }
        // Cell images, shifted to their column.
        let mut x = 0u16;
        let mut first = true;
        for (inner, w) in cells {
            if w == 0 {
                continue;
            }
            if !first {
                x += gap as u16;
            }
            first = false;
            for p in inner.placements {
                out.placements.push(Placement { line: p.line + base, x: p.x + x, ..p });
            }
            x += w as u16;
        }
        if border && header {
            let table_w = widths.iter().sum::<usize>() + t.gap_total(border, &maxs);
            out.lines.push(StyledLine { spans: vec![dim("─".repeat(table_w.min(width as usize)), None)] });
        }
    }
    out.gap();
}

/// Distributes width among columns: those that fit at their natural width
/// within a fair share get it in full; the rest is shared among the others in
/// proportion to what they are missing.
fn distribute(mins: &[u16], maxs: &[u16], avail: usize) -> Vec<usize> {
    let sum_max: usize = maxs.iter().map(|&m| m as usize).sum();
    let sum_min: usize = mins.iter().map(|&m| m as usize).sum();
    if sum_max <= avail {
        return maxs.iter().map(|&m| m as usize).collect();
    }
    if sum_min >= avail {
        // Not even the minimum fits: shrink proportionally.
        let mut w: Vec<usize> = mins.iter().map(|&m| if m == 0 { 0 } else { ((m as usize * avail) / sum_min.max(1)).max(1) }).collect();
        let mut total: usize = w.iter().sum();
        while total > avail {
            let i = (0..w.len()).max_by_key(|&i| w[i]).unwrap_or(0);
            if w[i] <= 1 {
                break;
            }
            w[i] -= 1;
            total -= 1;
        }
        return w;
    }
    let n = mins.len();
    let mut w = vec![0usize; n];
    let mut fixed = vec![false; n];
    let mut left = avail;
    loop {
        let open: Vec<usize> = (0..n).filter(|&i| !fixed[i] && maxs[i] > 0).collect();
        if open.is_empty() {
            break;
        }
        let share = left / open.len();
        let small: Vec<usize> = open.iter().copied().filter(|&i| (maxs[i] as usize) <= share).collect();
        if small.is_empty() {
            // The remaining ones don't fit in full: minimum + proportional share.
            let need_min: usize = open.iter().map(|&i| mins[i] as usize).sum();
            let extra = left.saturating_sub(need_min);
            let flex: usize = open.iter().map(|&i| (maxs[i] - mins[i]) as usize).sum();
            for &i in &open {
                w[i] = mins[i] as usize + ((maxs[i] - mins[i]) as usize * extra) / flex.max(1);
            }
            let used: usize = open.iter().map(|&i| w[i]).sum();
            if let Some(&i) = open.iter().max_by_key(|&&i| maxs[i] - mins[i]) {
                w[i] += left.saturating_sub(used);
            }
            break;
        }
        for i in small {
            w[i] = maxs[i] as usize;
            fixed[i] = true;
            left -= w[i];
        }
    }
    w
}
