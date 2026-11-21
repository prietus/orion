//! Simple HTML-to-text conversion that respects blocks, line breaks and lists.
//! Used as the plain-text fallback for HTML-only messages; the native renderer
//! lives in `orion-html`.

pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    let mut skip_depth = 0usize; // inside <script>/<style>/<head>
    let mut list_stack: Vec<Option<u32>> = Vec::new(); // None = ul, Some(n) = ol
    let mut pre = 0usize;
    let mut quote = 0usize;

    while !rest.is_empty() {
        if let Some(tag_start) = rest.find('<') {
            let (text, after) = rest.split_at(tag_start);
            if skip_depth == 0 {
                push_text(&mut out, &decode_entities(text), pre > 0, quote);
            }
            if after.starts_with("<!--") {
                rest = after.find("-->").map(|i| &after[i + 3..]).unwrap_or("");
                continue;
            }
            let Some(end) = after.find('>') else { break };
            let tag = &after[1..end];
            rest = &after[end + 1..];
            let closing = tag.starts_with('/');
            let name: String = tag
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            match name.as_str() {
                "script" | "style" | "head" | "title" => {
                    if closing { skip_depth = skip_depth.saturating_sub(1) } else if !tag.ends_with('/') { skip_depth += 1 }
                }
                _ if skip_depth > 0 => {}
                "br" => newline(&mut out, quote),
                "p" | "div" | "section" | "article" | "header" | "footer" | "table" | "form" => {
                    blank_line(&mut out, quote)
                }
                "tr" => newline(&mut out, quote),
                "td" | "th" if !closing => {
                    if !out.ends_with('\n') && !out.ends_with(' ') && !out.is_empty() {
                        out.push_str("  ");
                    }
                }
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    blank_line(&mut out, quote);
                    if !closing {
                        let level = name[1..].parse::<usize>().unwrap_or(1);
                        out.push_str(&"#".repeat(level));
                        out.push(' ');
                    }
                }
                "hr" => {
                    newline(&mut out, quote);
                    out.push_str("────────────────────────────────");
                    newline(&mut out, quote);
                }
                "ul" | "ol" => {
                    if closing {
                        list_stack.pop();
                    } else {
                        list_stack.push((name == "ol").then_some(0));
                    }
                    newline(&mut out, quote);
                }
                "li" if !closing => {
                    newline(&mut out, quote);
                    let depth = list_stack.len().max(1);
                    out.push_str(&"  ".repeat(depth - 1));
                    match list_stack.last_mut() {
                        Some(Some(n)) => {
                            *n += 1;
                            out.push_str(&format!("{n}. "));
                        }
                        _ => out.push_str("• "),
                    }
                }
                "pre" => {
                    if closing { pre = pre.saturating_sub(1) } else { pre += 1 }
                    newline(&mut out, quote);
                }
                "blockquote" => {
                    // Blank line at the outer level, and the next one already with the new prefix.
                    let outer = if closing { quote.saturating_sub(1) } else { quote };
                    let inner = if closing { outer } else { quote + 1 };
                    if !out.is_empty() {
                        newline(&mut out, outer);
                    }
                    newline(&mut out, inner);
                    quote = if closing { outer } else { inner };
                }
                "img" if !closing => {
                    if let Some(alt) = attr(tag, "alt").filter(|a| !a.is_empty()) {
                        out.push_str(&format!("[img: {}]", decode_entities(&alt)));
                    }
                }
                _ => {}
            }
        } else {
            if skip_depth == 0 {
                push_text(&mut out, &decode_entities(rest), pre > 0, quote);
            }
            break;
        }
    }
    // Collapse runs of more than one blank line.
    let mut result = String::with_capacity(out.len());
    let mut blank = 0;
    for line in out.lines() {
        let l = line.trim_end();
        if l.trim_start_matches(['>', ' ']).is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        result.push_str(l);
        result.push('\n');
    }
    result.trim_matches('\n').to_string()
}

fn push_text(out: &mut String, text: &str, pre: bool, quote: usize) {
    if pre {
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                newline(out, quote);
            }
            out.push_str(line);
        }
        return;
    }
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space && !out.is_empty() && !out.ends_with(['\n', ' ']) {
            out.push(' ');
        }
        pending_space = false;
        out.push(c);
    }
    if pending_space && !out.is_empty() && !out.ends_with(['\n', ' ']) {
        out.push(' ');
    }
}

fn newline(out: &mut String, quote: usize) {
    while out.ends_with(' ') {
        out.pop();
    }
    out.push('\n');
    if quote > 0 {
        out.push_str(&"> ".repeat(quote));
    }
}

fn blank_line(out: &mut String, quote: usize) {
    if out.is_empty() {
        return;
    }
    newline(out, quote);
    newline(out, quote);
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let pos = lower.find(&format!("{name}="))?;
    let v = &tag[pos + name.len() + 1..];
    let (q, v) = match v.chars().next()? {
        c @ ('"' | '\'') => (Some(c), &v[1..]),
        _ => (None, v),
    };
    let end = match q {
        Some(q) => v.find(q)?,
        None => v.find(char::is_whitespace).unwrap_or(v.len()),
    };
    Some(v[..end].to_string())
}

pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[..rest.len().min(12)].find(';');
        let decoded = end.and_then(|e| {
            let ent = &rest[1..e];
            let ch = if let Some(num) = ent.strip_prefix('#') {
                let n = match num.strip_prefix(['x', 'X']) {
                    Some(h) => u32::from_str_radix(h, 16).ok(),
                    None => num.parse().ok(),
                };
                n.and_then(char::from_u32)
            } else {
                Some(match ent {
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    "nbsp" => ' ',
                    "copy" => '©',
                    "reg" => '®',
                    "euro" => '€',
                    "mdash" => '—',
                    "ndash" => '–',
                    "hellip" => '…',
                    "laquo" => '«',
                    "raquo" => '»',
                    "iexcl" => '¡',
                    "iquest" => '¿',
                    "aacute" => 'á', "eacute" => 'é', "iacute" => 'í', "oacute" => 'ó', "uacute" => 'ú',
                    "Aacute" => 'Á', "Eacute" => 'É', "Iacute" => 'Í', "Oacute" => 'Ó', "Uacute" => 'Ú',
                    "ntilde" => 'ñ', "Ntilde" => 'Ñ', "uuml" => 'ü', "Uuml" => 'Ü', "ccedil" => 'ç',
                    "zwnj" | "zwj" | "shy" => '\u{200b}',
                    _ => return None,
                })
            };
            ch.map(|c| (c, e + 1))
        });
        match decoded {
            Some((c, len)) => {
                if c != '\u{200b}' {
                    out.push(c);
                }
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_and_lists() {
        let t = html_to_text(
            "<html><head><style>p{}</style></head><body><h1>News</h1><p>It is <b>only</b> HTML.</p>\
             <ul><li>One</li><li>Two</li></ul><ol><li>a</li><li>b</li></ol><p>A &amp; B&nbsp;&#233;</p></body></html>",
        );
        assert_eq!(t, "# News\n\nIt is only HTML.\n\n• One\n• Two\n\n1. a\n2. b\n\nA & B é");
    }

    #[test]
    fn quotes_and_breaks() {
        let t = html_to_text("hello<br>world<blockquote>quoted</blockquote>end");
        assert_eq!(t, "hello\nworld\n\n> quoted\n\nend");
    }
}
