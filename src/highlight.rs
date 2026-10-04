//! Syntax highlighting shared by the diff overlay and the file viewer: the bat syntax set from
//! two-face, OneHalfDark colours, one span list per line.

use std::sync::OnceLock;

use ratatui::style::Color;
use ratatui::text::Span;
use two_face::re_exports::syntect::easy::HighlightLines;
use two_face::re_exports::syntect::parsing::SyntaxSet;
use two_face::re_exports::syntect::util::LinesWithEndings;
use two_face::theme::EmbeddedThemeName;

/// One line as coloured text runs.
pub type Styled = Vec<(Color, String)>;

/// Highlighting is line-by-line regex work; past this size the text is shown plain.
const MAX_HIGHLIGHT: usize = 512 * 1024;

/// Colours `text` by the syntax for `path`'s extension (or name, or first line).
pub fn highlight(ss: &SyntaxSet, path: &str, text: &str) -> Vec<Styled> {
    let plain = |l: &str| vec![(Color::Reset, clean(l))];
    if text.len() > MAX_HIGHLIGHT {
        return LinesWithEndings::from(text).map(plain).collect();
    }
    let file = path.rsplit('/').next().unwrap_or(path);
    let ext = file.rsplit_once('.').map_or(file, |(_, e)| e);
    let syntax = ss
        .find_syntax_by_extension(ext)
        .or_else(|| ss.find_syntax_by_extension(file))
        .or_else(|| text.lines().next().and_then(|l| ss.find_syntax_by_first_line(l)))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    static THEMES: OnceLock<two_face::theme::EmbeddedLazyThemeSet> = OnceLock::new();
    let themes = THEMES.get_or_init(two_face::theme::extra);
    let mut h = HighlightLines::new(syntax, themes.get(EmbeddedThemeName::OneHalfDark));
    LinesWithEndings::from(text)
        .map(|l| match h.highlight_line(l, ss) {
            Ok(spans) => spans
                .into_iter()
                .map(|(s, t)| (Color::Rgb(s.foreground.r, s.foreground.g, s.foreground.b), clean(t)))
                .filter(|(_, t)| !t.is_empty())
                .collect(),
            Err(_) => plain(l),
        })
        .collect()
}

/// Tabs render as 4 spaces; line endings are dropped.
pub fn clean(s: &str) -> String {
    s.trim_end_matches(['\n', '\r']).replace('\t', "    ")
}

/// Drops the first `left` characters across spans, for horizontal scrolling.
pub fn shift(spans: &[(Color, String)], mut left: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for (c, t) in spans {
        let n = t.chars().count();
        if left >= n {
            left -= n;
            continue;
        }
        out.push(Span::styled(t.chars().skip(left).collect::<String>(), *c));
        left = 0;
    }
    out
}
