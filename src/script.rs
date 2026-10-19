//! Parsing a lettering script: one line of dialogue per row, tagged by a style prefix.
//!
//! A letterer pastes a chapter into the panel and writes it the way they already write it:
//!
//! ```text
//! d: I'll be home by six.
//! ## (a note to myself, not dialogue)
//! s: You said that yesterday.
//! // and still yesterday.
//! ```
//!
//! The first token that matches a style's prefix picks that style for the line; a line whose prefix
//! is in the ignore list, that is a `Page N` marker, or that is blank once its prefixes are off, is
//! not dialogue and is skipped by the line navigation. What follows ports **TypeR 2.5.0**'s rules
//! (`app_src/context.jsx`'s reducer) exactly, including the ones that surprise people:
//!
//! * matching is **case sensitive** and against the **raw, untrimmed** line, so ` d:` on an indented
//!   line does not match the prefix `d:` — it matches nothing;
//! * the **first** prefix in library order wins, not the longest: a `#` prefix on an earlier style
//!   shadows `##` on a later one;
//! * exactly **one** style prefix is taken off a line, and one ignore prefix;
//! * `//` and `//:` **inherit the style of the last styled dialogue line**, which is not necessarily
//!   the row above: an ignored note or an untagged line in between does not change what is inherited,
//!   and a `//` line before any styled line inherits nothing (and takes the default style).
//! * with **folder priority** on (TypeR's default), a prefix belonging to the folder of the style in
//!   use wins over every other prefix in the library; with it off, a prefix on a style in no folder
//!   wins first, then any foldered prefix. Either way the whole library is the last resort.
//!
//! Three rules are deliberately *not* ported, all for the same reason — each one is a way for a
//! script to lose text the letterer did not ask to lose:
//!
//! * **A prefix is taken off the front, not out of the middle.** TypeR does
//!   `rawText.replace(prefix, '')`, which removes *every* occurrence anywhere in the line, so
//!   `# I said #1!` comes out of the original as `I said 1!`.
//! * **An empty prefix matches nothing.** In the original `rawText.startsWith('')` is true for every
//!   line, so an empty style prefix would claim the whole script.
//! * **A pasted script has no carriage returns.** The original splits on `\n` alone and leaves a
//!   `\r` at the end of every Windows line.
//!
//! Markdown is *not* parsed here. The stored line keeps its markers; [`markdown`] turns a line into
//! runs at the moment it is lettered, exactly as TypeR does (`utils.js`'s `parseMarkdownRuns`).

/// One row of the script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// The line as it was typed, without its line ending.
    pub raw: String,
    /// The dialogue: the raw line with its ignore prefix, its style prefix and its ignore tags taken
    /// off, trimmed. Markdown markers are still in it.
    pub text: String,
    /// The style the line uses: the style its prefix names, or the one `//` inherited.
    pub style_id: Option<String>,
    /// The ignore prefix that matched, if one did.
    pub ignore_prefix: Option<String>,
    /// The line continues the previous line's style (`//` or `//:`).
    pub inherits: bool,
    /// A `Page N` marker: a page break in the script, never dialogue.
    pub page: bool,
    /// Not dialogue: it carries an ignore prefix, is a page marker, or nothing is left of it.
    pub ignored: bool,
    /// The line's number among the dialogue lines, from 1. Zero for an ignored line — which is why
    /// it is not a simple count of the rows above.
    pub number: usize,
}

/// A prefix that picks a style: `(style id, prefix)`.
pub type StylePrefix = (String, String);

/// Every prefix a script can match, in the three orders TypeR tries them in.
///
/// The orders are the caller's ([`crate::library::Library::prefixes`]), because they depend on the
/// library's folders and on the style in use — not on the script.
#[derive(Clone, Debug, Default)]
pub struct Prefixes {
    /// Every style's prefixes in library order: the last resort, and the order that decides which of
    /// two styles sharing a prefix wins.
    pub all: Vec<StylePrefix>,
    /// The prefixes of styles in the folder of the style in use.
    pub in_current_folder: Vec<StylePrefix>,
    /// The prefixes of styles in no folder.
    pub unsorted: Vec<StylePrefix>,
    /// The prefixes of styles in a folder.
    pub foldered: Vec<StylePrefix>,
    /// Whether the current style's folder wins over everything else (TypeR's
    /// `currentFolderTagPriority`, on by default).
    pub folder_priority: bool,
}

impl Prefixes {
    /// The prefix that picks a style for `raw`, in TypeR's order of preference.
    ///
    /// With folder priority on, the style's own folder is tried first and the whole library second.
    /// With it off, the unsorted styles are tried first, then the foldered ones, then the library.
    /// An empty prefix never matches (see the module note).
    fn find(&self, raw: &str) -> Option<&StylePrefix> {
        if self.folder_priority {
            hit(&self.in_current_folder, raw).or_else(|| hit(&self.all, raw))
        } else {
            hit(&self.unsorted, raw).or_else(|| hit(&self.foldered, raw)).or_else(|| hit(&self.all, raw))
        }
    }
}

/// The first prefix in `list` that `raw` starts with.
fn hit<'a>(list: &'a [StylePrefix], raw: &str) -> Option<&'a StylePrefix> {
    list.iter().find(|(_, p)| !p.is_empty() && raw.starts_with(p.as_str()))
}

/// The script-side settings a parse depends on.
#[derive(Clone, Copy, Debug)]
pub struct Rules<'a> {
    /// TypeR's `ignoreLinePrefixes` (default `["##"]`).
    pub ignore_prefixes: &'a [String],
    /// TypeR's `ignoreTags`: tokens removed from the dialogue wherever they appear (default none).
    pub ignore_tags: &'a [String],
    pub prefixes: &'a Prefixes,
}

/// Splits `text` into lines. Only `\n` separates lines, as in TypeR; a lone `\r` (an old Mac file,
/// or a stray one) is treated as one too rather than left inside the line.
pub fn split(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l))
}

/// Parses a whole script.
pub fn parse(text: &str, rules: &Rules<'_>) -> Vec<Line> {
    let mut number = 0;
    // The style the last *styled dialogue line* carried: what `//` inherits. Not "the previous row",
    // which is why an ignored note or an untagged line in between changes nothing (TypeR sets this
    // only for a line that is not ignored and has a style).
    let mut previous: Option<String> = None;
    split(text)
        .map(|raw| {
            let ignore_prefix = rules
                .ignore_prefixes
                .iter()
                .find(|p| !p.is_empty() && raw.starts_with(p.as_str()))
                .cloned();
            let is_continuation = raw.starts_with("//");
            // `//` beats any prefix that would otherwise match, exactly as in TypeR's `if`.
            let matched = if is_continuation { None } else { rules.prefixes.find(raw) };
            let marker = if is_continuation { Some(if raw.starts_with("//:") { "//:" } else { "//" }) } else { None };

            let mut rest = raw;
            if let Some(p) = &ignore_prefix {
                rest = rest.strip_prefix(p.as_str()).unwrap_or(rest);
            }
            if let Some(m) = marker {
                rest = rest.strip_prefix(m).unwrap_or(rest);
            } else if let Some((_, p)) = matched {
                rest = rest.strip_prefix(p.as_str()).unwrap_or(rest);
            }
            // Ignore tags come out of what is left, wherever they are: TypeR's
            // `ignoreTags.reduce((acc, tag) => acc.split(tag).join(""), text)`. An empty tag is
            // skipped, as it is there.
            let mut text = rest.to_string();
            for tag in rules.ignore_tags.iter().filter(|t| !t.is_empty()) {
                text = text.replace(tag.as_str(), "");
            }
            let text = text.trim().to_string();

            let page = is_page_marker(raw);
            let ignored = ignore_prefix.is_some() || page || text.is_empty();
            if !ignored {
                number += 1;
            }
            let style_id = if is_continuation { previous.clone() } else { matched.map(|(id, _)| id.clone()) };
            let line = Line {
                raw: raw.to_string(),
                text,
                style_id,
                ignore_prefix,
                inherits: is_continuation,
                page,
                ignored,
                number: if ignored { 0 } else { number },
            };
            if !line.ignored && line.style_id.is_some() {
                previous.clone_from(&line.style_id);
            }
            line
        })
        .collect()
}

/// TyperTools' and TypeR's page marker, `rawText.match(/Page [0-9]+/i)`: not anchored, so a row
/// that says "See Page 5" is a page break too.
pub fn is_page_marker(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let mut i = 0;
    loop {
        let Some(window) = bytes.get(i..i + 5) else { return false };
        if window.eq_ignore_ascii_case(b"page ") && bytes.get(i + 5).is_some_and(u8::is_ascii_digit) {
            return true;
        }
        i += 1;
    }
}

/// The number of dialogue lines: what the line field counts up to.
pub fn count(lines: &[Line]) -> usize {
    lines.iter().filter(|l| !l.ignored).count()
}

/// The raw index of the `n`th dialogue line, counting from 0 — and its neighbours, for the ← and →
/// buttons. Both skip ignored lines, and neither runs off either end.
pub fn nth(lines: &[Line], n: usize) -> Option<usize> {
    lines.iter().enumerate().filter(|(_, l)| !l.ignored).map(|(i, _)| i).nth(n)
}

/// The next dialogue line after `raw`, or `None` at the end.
pub fn next(lines: &[Line], raw: usize) -> Option<usize> {
    ((raw + 1)..lines.len()).find(|&i| !lines[i].ignored)
}

/// The previous dialogue line before `raw`, or `None` at the start.
pub fn previous(lines: &[Line], raw: usize) -> Option<usize> {
    (0..raw).rev().find(|&i| !lines[i].ignored)
}

/// Where the line field should point when the script changed: the same row, as long as the script
/// still has one, and the first dialogue line when it does not.
///
/// A row is kept even when it is not dialogue. The letterer is looking at it, and typing above it
/// must not move it; an ignored row is also a real answer — "this line is a stage direction" — which
/// is worth saying rather than stepping over. The arrows and the line field are what skip them.
pub fn settle(lines: &[Line], wanted: usize) -> Option<usize> {
    match lines.get(wanted) {
        Some(_) => Some(wanted),
        None => nth(lines, 0),
    }
}

// ---------------------------------------------------------------- markdown

/// One stretch of a line with one weight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    /// How many characters of the plain text the run covers.
    pub chars: usize,
    pub bold: bool,
    pub italic: bool,
}

/// A line with its markdown taken off, and the weights the markers asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Markdown {
    /// The text without its markers, unescaped: what the type layer holds.
    pub plain: String,
    /// The weighted stretches, adjacent equal ones merged. Empty when nothing was marked.
    pub runs: Vec<Run>,
}

impl Markdown {
    /// Whether any character is bold or italic.
    pub fn has_formatting(&self) -> bool {
        self.runs.iter().any(|r| r.bold || r.italic)
    }
}

/// The markers TypeR recognises, longest first in each family so `***` beats `**` beats `*` at the
/// same position (`utils.js`'s `MARKDOWN_MARKERS`).
struct Marker {
    token: &'static str,
    bold: bool,
    italic: bool,
}

const MARKERS: [Marker; 6] = [
    Marker { token: "***", bold: true, italic: true },
    Marker { token: "___", bold: true, italic: true },
    Marker { token: "**", bold: true, italic: false },
    Marker { token: "__", bold: true, italic: false },
    Marker { token: "*", bold: false, italic: true },
    Marker { token: "_", bold: false, italic: true },
];

/// The deepest nesting [`parse_markdown`] will open. TypeR recurses without a limit; a pasted script
/// is input, and this port must not be driven into a stack overflow by it. Past this depth the rest
/// of the line is taken as plain text, markers included.
const MAX_DEPTH: usize = 32;

/// Splits a line into runs the way TypeR's `parseMarkdownRuns` does.
///
/// Recognised: `***`/`___` bold italic, `**`/`__` bold, `*`/`_` italic. A backslash escapes `\`,
/// `*` and `_`; an unclosed marker is literal text. The text is unescaped on the way out, so what is
/// left is exactly what the letterer will see.
pub fn parse_markdown(input: &str) -> Markdown {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Markdown::default();
    walk(&chars, 0, chars.len(), false, false, 0, &mut out);
    out
}

/// Appends `s` to the plain text, merging into the last run when the weight is the same.
fn push_run(out: &mut Markdown, s: &str, bold: bool, italic: bool) {
    if s.is_empty() {
        return;
    }
    out.plain.push_str(s);
    let chars = s.chars().count();
    if let Some(last) = out.runs.last_mut()
        && last.bold == bold
        && last.italic == italic
    {
        last.chars = last.chars.saturating_add(chars);
        return;
    }
    out.runs.push(Run { chars, bold, italic });
}

/// Unescapes `\\`, `\*` and `\_`, leaving every other backslash as it is.
fn unescape(segment: &[char]) -> String {
    let mut s = String::with_capacity(segment.len());
    let mut i = 0;
    while i < segment.len() {
        if segment[i] == '\\' && matches!(segment.get(i + 1), Some('\\' | '*' | '_')) {
            if let Some(c) = segment.get(i + 1) {
                s.push(*c);
            }
            i += 2;
            continue;
        }
        s.push(segment[i]);
        i += 1;
    }
    s
}

/// Whether the character at `index` is escaped: an odd number of backslashes before it.
fn is_escaped(chars: &[char], index: usize) -> bool {
    let mut backslashes = 0;
    let mut i = index;
    while i > 0 && chars[i - 1] == '\\' {
        backslashes += 1;
        i -= 1;
    }
    backslashes % 2 == 1
}

/// The first unescaped occurrence of `token` at or after `start`.
fn find_token(chars: &[char], token: &[char], start: usize) -> Option<usize> {
    if token.is_empty() {
        return None;
    }
    let mut i = start;
    loop {
        let window = chars.get(i..i + token.len())?;
        if window == token && !is_escaped(chars, i) {
            return Some(i);
        }
        i += 1;
    }
}

/// The earliest unescaped marker at or after `start`; on a tie the longer token wins (TypeR's
/// `findNextMarker`), which is what makes `***x***` bold italic rather than bold-then-italic.
fn next_marker(chars: &[char], start: usize) -> Option<(usize, &'static Marker)> {
    let mut best: Option<(usize, &'static Marker)> = None;
    for marker in &MARKERS {
        let token: Vec<char> = marker.token.chars().collect();
        let Some(index) = find_token(chars, &token, start) else { continue };
        let better = match best {
            None => true,
            Some((at, m)) => index < at || (index == at && marker.token.len() > m.token.len()),
        };
        if better {
            best = Some((index, marker));
        }
    }
    best
}

#[allow(clippy::too_many_arguments)]
fn walk(chars: &[char], from: usize, to: usize, bold: bool, italic: bool, depth: usize, out: &mut Markdown) {
    let mut cursor = from;
    while cursor < to {
        let Some((at, marker)) = next_marker(&chars[..to], cursor) else {
            push_run(out, &unescape(&chars[cursor..to]), bold, italic);
            return;
        };
        if at > cursor {
            push_run(out, &unescape(&chars[cursor..at]), bold, italic);
        }
        let token: Vec<char> = marker.token.chars().collect();
        let after_open = at + token.len();
        let Some(close) = find_token(&chars[..to], &token, after_open) else {
            // Unclosed: the marker is literal text, and scanning carries on after it.
            push_run(out, marker.token, bold, italic);
            cursor = after_open;
            continue;
        };
        if depth >= MAX_DEPTH {
            // Too deep to open another: the markers stay in the text rather than nesting further.
            push_run(out, &unescape(&chars[at..close + token.len()]), bold, italic);
            cursor = close + token.len();
            continue;
        }
        walk(chars, after_open, close, bold || marker.bold, italic || marker.italic, depth + 1, out);
        cursor = close + token.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat() -> Prefixes {
        Prefixes {
            all: vec![
                ("dialogue".into(), "d:".into()),
                ("shout".into(), "#".into()),
                ("whisper".into(), "##".into()),
                ("thought".into(), "t:".into()),
            ],
            folder_priority: true,
            ..Prefixes::default()
        }
    }

    fn ignored() -> Vec<String> {
        vec!["##".into()]
    }

    fn rules<'a>(prefixes: &'a Prefixes, ignore: &'a [String], tags: &'a [String]) -> Rules<'a> {
        Rules { ignore_prefixes: ignore, ignore_tags: tags, prefixes }
    }

    fn parsed(text: &str) -> Vec<Line> {
        let p = flat();
        let tags: Vec<String> = Vec::new();
        parse(text, &rules(&p, &ignored(), &tags))
    }

    #[test]
    fn numbers_count_dialogue_lines_only() {
        let lines = parsed("d: One\n\n## a note\nd: Two\nt: Three");
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0].number, 1);
        assert!(lines[1].ignored, "a blank line is not dialogue");
        assert_eq!(lines[1].number, 0);
        assert!(lines[2].ignored);
        assert_eq!(lines[3].number, 2, "the numbering ignores the rows above it");
        assert_eq!(lines[4].number, 3);
        assert_eq!(count(&lines), 3);
    }

    #[test]
    fn a_prefix_is_taken_off_the_line_and_names_the_style() {
        let lines = parsed("d: I'll be home by six.\n t: indented");
        assert_eq!(lines[0].text, "I'll be home by six.");
        assert_eq!(lines[0].style_id.as_deref(), Some("dialogue"));
        // Leading space: the raw line does not start with `t:`, so no prefix matches and the whole
        // line is the dialogue. A letterer indenting a line means to indent it.
        assert_eq!(lines[1].text, "t: indented");
        assert_eq!(lines[1].style_id, None);
        assert!(!lines[1].ignored);
    }

    #[test]
    fn the_first_prefix_in_library_order_wins_not_the_longest() {
        let lines = parse("## Careful!", &rules(&flat(), &[], &[]));
        assert_eq!(lines[0].style_id.as_deref(), Some("shout"));
        assert_eq!(lines[0].text, "# Careful!");
        assert!(!lines[0].ignored);
        // With the ignore list in the way, the ignore prefix takes precedence over the style.
        let lines = parsed("## Careful!");
        assert!(lines[0].ignored);
        assert_eq!(lines[0].text, "Careful!");
    }

    #[test]
    fn a_double_slash_inherits_the_last_styled_line_not_the_row_above() {
        let lines = parsed("d: One\n## note\nx untagged\nt: Two\n//: two again");
        assert!(lines[1].ignored);
        // An ignored line is still *matched* against the prefixes — TypeR computes the style before
        // it decides the line is ignored — but it never becomes what `//` inherits.
        assert_eq!(lines[1].style_id.as_deref(), Some("shout"), "`#` shadows `##` in the library order");
        assert_eq!(lines[2].style_id, None, "an untagged line carries no style either");
        assert_eq!(lines[3].style_id.as_deref(), Some("thought"));
        assert_eq!(lines[4].style_id.as_deref(), Some("thought"), "`//:` follows line 3, not line 1");
        assert_eq!(lines[4].text, "two again");
        // A `//` line right after an ignored styled line still follows the last *dialogue* style.
        let lines = parsed("d: One\n## note\n// still one");
        assert_eq!(lines[2].style_id.as_deref(), Some("dialogue"), "the note did not become the inherited style");
        assert_eq!(lines[2].text, "still one");
        // And an inherited style is itself carried forward, as in TypeR.
        let lines = parsed("d: One\nd: Two\n// three\n// four");
        assert_eq!(lines[2].style_id.as_deref(), Some("dialogue"));
        assert_eq!(lines[3].style_id.as_deref(), Some("dialogue"));
    }

    #[test]
    fn a_double_slash_before_any_style_inherits_nothing_but_is_dialogue() {
        let lines = parsed("// no style yet\nd: One");
        assert!(!lines[0].ignored);
        assert_eq!(lines[0].style_id, None, "there is nothing to inherit, so the default style applies");
        assert_eq!(lines[0].text, "no style yet");
        // `//` with nothing after it is still not dialogue.
        let lines = parsed("d: One\n//\n//:");
        assert!(lines[1].ignored);
        assert!(lines[2].ignored);
    }

    #[test]
    fn a_double_slash_beats_a_style_prefix_that_would_match() {
        let mut p = flat();
        p.all.push(("slash".into(), "//".into()));
        let lines = parse("d: One\n//: text", &rules(&p, &[], &[]));
        assert_eq!(lines[1].style_id.as_deref(), Some("dialogue"), "inherited, not the `//` style");
        assert_eq!(lines[1].text, "text");
    }

    #[test]
    fn ignore_tags_come_out_of_the_dialogue_wherever_they_are() {
        let p = flat();
        let tags = vec!["{b}".into(), String::new(), "{/b}".into()];
        let lines = parse("d: {b}Hi{/b} there", &rules(&p, &[], &tags));
        assert_eq!(lines[0].text, "Hi there");
        assert!(!lines[0].ignored);
        // They are removed from what is left after the prefix, not from the raw line, and an empty
        // tag is skipped rather than matching everywhere.
        let lines = parse("d: a {x} b {x}", &rules(&p, &[], &["".into(), "{x}".into()]));
        assert_eq!(lines[0].text, "a  b");
    }

    #[test]
    fn a_page_marker_is_not_dialogue() {
        let lines = parsed("Page 3\nd: One\nSee Page 12 for more\nd: Two");
        assert!(lines[0].ignored && lines[0].page);
        assert!(lines[2].page, "the match is not anchored: a sentence naming a page is a marker too");
        assert!(lines[2].ignored);
        assert_eq!(lines[1].number, 1);
        assert_eq!(lines[3].number, 2);
        // Case-insensitive, and a number is required.
        assert!(is_page_marker("page 7"));
        assert!(!is_page_marker("Page seven"));
        assert!(!is_page_marker("Pagination 4"));
    }

    #[test]
    fn the_style_in_use_gives_its_folder_priority() {
        // The library: a shared prefix `#` on a style in "balloons", and another `#` in "effects".
        let mut p = Prefixes {
            all: vec![("effects".into(), "#".into()), ("balloons".into(), "#".into())],
            in_current_folder: vec![("balloons".into(), "#".into())],
            unsorted: vec![],
            foldered: vec![("effects".into(), "#".into()), ("balloons".into(), "#".into())],
            folder_priority: true,
        };
        let lines = parse("# Bang", &rules(&p, &[], &[]));
        assert_eq!(lines[0].style_id.as_deref(), Some("balloons"), "the folder in use wins");
        // With priority off, the unsorted styles are tried first, then the foldered ones in order.
        p.folder_priority = false;
        let lines = parse("# Bang", &rules(&p, &[], &[]));
        assert_eq!(lines[0].style_id.as_deref(), Some("effects"), "the first foldered style wins");
        p.unsorted = vec![("shout".into(), "#".into())];
        let lines = parse("# Bang", &rules(&p, &[], &[]));
        assert_eq!(lines[0].style_id.as_deref(), Some("shout"), "and an unsorted style beats a folder");
        // Either way the whole library is the last resort.
        p.unsorted.clear();
        p.foldered.clear();
        let lines = parse("# Bang", &rules(&p, &[], &[]));
        assert_eq!(lines[0].style_id.as_deref(), Some("effects"));
    }

    #[test]
    fn an_empty_prefix_matches_nothing() {
        let p = Prefixes { all: vec![("dialogue".into(), String::new())], folder_priority: true, ..Prefixes::default() };
        let lines = parse("plain", &rules(&p, &[String::new()], &[]));
        assert_eq!(lines[0].style_id, None);
        assert!(!lines[0].ignored);
    }

    #[test]
    fn a_script_pasted_from_windows_has_no_carriage_returns() {
        let lines = parsed("d: One\r\nd: Two\r\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].raw, "d: One");
        assert_eq!(lines[1].text, "Two");
        assert!(lines[2].ignored, "the trailing newline leaves one empty row");
    }

    #[test]
    fn a_prefix_longer_than_the_line_does_not_match() {
        let lines = parsed("d");
        assert_eq!(lines[0].text, "d");
        assert_eq!(lines[0].style_id, None);
        let lines = parsed("d:");
        assert!(lines[0].ignored, "prefix, nothing left");
        assert_eq!(lines[0].style_id.as_deref(), Some("dialogue"));
    }

    #[test]
    fn navigation_skips_the_lines_that_are_not_dialogue() {
        let lines = parsed("d: One\n## note\nPage 2\nd: Two\n\nd: Three");
        assert_eq!(nth(&lines, 0), Some(0));
        assert_eq!(nth(&lines, 1), Some(3));
        assert_eq!(nth(&lines, 2), Some(5));
        assert_eq!(nth(&lines, 3), None);
        assert_eq!(next(&lines, 0), Some(3));
        assert_eq!(next(&lines, 3), Some(5));
        assert_eq!(next(&lines, 5), None);
        assert_eq!(previous(&lines, 5), Some(3));
        assert_eq!(previous(&lines, 0), None);
    }

    #[test]
    fn a_position_that_is_gone_falls_back_to_the_first_line() {
        let lines = parsed("d: One\nd: Two");
        assert_eq!(settle(&lines, 1), Some(1), "a position that still holds is kept");
        assert_eq!(settle(&lines, 7), Some(0), "one that does not is the first line");
        let with_blank = parsed("d: One\n\nd: Two");
        assert_eq!(settle(&with_blank, 1), Some(1), "an ignored row is kept where it is");
        assert_eq!(settle(&parsed("\n\nd: Two"), 0), Some(0), "even at the top of the script");
    }

    // --- markdown ------------------------------------------------------------------------------

    fn md(s: &str) -> Markdown {
        parse_markdown(s)
    }

    #[test]
    fn markdown_turns_markers_into_weighted_runs() {
        let m = md("I said **stop** now");
        assert_eq!(m.plain, "I said stop now");
        assert!(m.has_formatting());
        assert_eq!(m.runs, [Run { chars: 7, bold: false, italic: false }, Run { chars: 4, bold: true, italic: false }, Run { chars: 4, bold: false, italic: false }]);

        let m = md("*a* __b__ ***c***");
        assert_eq!(m.plain, "a b c");
        assert_eq!(m.runs, [
            Run { chars: 1, bold: false, italic: true },
            Run { chars: 1, bold: false, italic: false },
            Run { chars: 1, bold: true, italic: false },
            Run { chars: 1, bold: false, italic: false },
            Run { chars: 1, bold: true, italic: true },
        ]);
    }

    #[test]
    fn markdown_merges_adjacent_runs_of_the_same_weight() {
        let m = md("**a****b**");
        assert_eq!(m.plain, "ab");
        assert_eq!(m.runs, [Run { chars: 2, bold: true, italic: false }]);
    }

    #[test]
    fn an_escaped_marker_is_text() {
        let m = md(r"5 \* 3 and \\ and \_");
        assert_eq!(m.plain, r"5 * 3 and \ and _");
        assert!(!m.has_formatting());
        // An even number of backslashes leaves the marker live.
        let m = md(r"**bold** \\*italic*");
        assert_eq!(m.plain, r"bold \italic");
        assert_eq!(m.runs.iter().filter(|r| r.bold).count(), 1);
        assert_eq!(m.runs.iter().filter(|r| r.italic).count(), 1);
    }

    #[test]
    fn an_unclosed_marker_is_literal_text() {
        let m = md("a ** b");
        assert_eq!(m.plain, "a ** b");
        assert!(!m.has_formatting());
        let m = md("**a");
        assert_eq!(m.plain, "**a");
    }

    #[test]
    fn nested_markers_combine_their_weight() {
        let m = md("**a *b* c**");
        assert_eq!(m.plain, "a b c");
        assert_eq!(m.runs, [
            Run { chars: 2, bold: true, italic: false },
            Run { chars: 1, bold: true, italic: true },
            Run { chars: 2, bold: true, italic: false },
        ]);
    }

    #[test]
    fn absurd_nesting_does_not_recurse_forever() {
        // A pasted line can hold any number of markers. Past the depth cap they stay in the text, and
        // nothing overflows the stack.
        let deep = "*".repeat(200) + "x" + &"*".repeat(200);
        let m = parse_markdown(&deep);
        assert!(m.plain.contains('x'));
        let _ = m.has_formatting();
    }
}
