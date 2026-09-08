//! Words and whitespace.
//!
//! Text is printed RAW (escapes and entities as written);
//! only the decisions below touch it:
//! - `*` / `_` runs that could open or close emphasis get escaped inside emphasis / strong
//! - a lone `===` / `---` word starting a line gets escaped (it would read as a setext underline)
//! - a line break before a word that would start a block (`-`, `1.`, `#`, `>`) never breaks
//!
//! Around Chinese / Japanese text a whitespace follows Prettier's `printWhitespace`:
//! a line break between CJ characters is not a space, and a space next to one never breaks
//! (`cjk` classifies the word edges).

use std::borrow::Cow;

use oxc_formatter_core::arena_cow_str;
use oxc_markdown_parser::{Constructs, attention, lexical};

use crate::options::ProseWrap;

use super::{
    MarkdownFormatter,
    cjk::{self, Edge, Kind},
    inline::{Parts, Sep},
};

/// The word after a whitespace, when the next sibling is a text.
#[derive(Clone, Copy)]
pub struct NextWord<'a> {
    pub word: &'a str,
    /// The word is its text's only one.
    pub alone_on_line: bool,
}

#[derive(Clone, Copy, Default)]
pub struct TextContext<'a> {
    /// The text is the first / last child of an `Emphasis` / `Strong`: its marker as printed.
    pub first_of_delimiter: Option<u8>,
    pub last_of_delimiter: Option<u8>,
    /// The character right before / after the text as printed:
    /// a space for a line break, the neighbor node's edge character, the enclosing marker;
    /// `None` at a paragraph edge.
    pub edge_prev: Option<char>,
    pub edge_next: Option<char>,
    /// The previous sibling is a soft break (the text starts a source line).
    pub after_soft_break: bool,
    /// The next sibling is a soft break (the text ends a source line).
    pub before_soft_break: bool,
    /// Nothing follows the text on its source line (a node glued after it would be part of the line).
    pub ends_line: bool,
    /// The first word of the next sibling text, for the whitespace that ends this text.
    pub next_word: Option<NextWord<'a>>,
    /// The last word of the previous sibling text (a soft break's other side), for the CJK rules only;
    /// a trailing `\` there was escaped by the text (see `push_text`).
    pub prev_word: Option<&'a str>,
    /// The sentence puts spaces between CJ and non-CJK words (Prettier's `usesCJSpaces`);
    /// `None` when it has no CJK text at all.
    pub cj_spaces: Option<bool>,
    /// Inside a reference link's raw content, where a line break never prevents a break.
    pub is_link: bool,
}

/// Word separators: space, tab, newline.
pub fn is_split_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | ' ')
}

/// Splits `raw` into words and whitespace and pushes them, in one pass.
pub fn push_text<'a>(
    raw: &'a str,
    cx: TextContext<'a>,
    parts: &mut Parts<'a>,
    f: &MarkdownFormatter<'_, 'a>,
) {
    let in_delimiter = f.context().delimiter_depth().get() > 0;
    let prose_wrap = f.options().prose_wrap;

    let mut rest = raw;
    let mut is_first = true;
    let leading_ws = raw.starts_with(is_split_whitespace);
    let mut prev_word: Option<&'a str> = None;
    loop {
        // Whitespace before the next word (leading, or the run after the previous word).
        let after_ws = rest.trim_start_matches(is_split_whitespace);
        let ws = &rest[..rest.len() - after_ws.len()];
        rest = after_ws;
        let word_end = rest.find(is_split_whitespace).unwrap_or(rest.len());
        let (word, tail) = rest.split_at(word_end);
        let is_last = tail.trim_start_matches(is_split_whitespace).is_empty();
        if !ws.is_empty() {
            let next = if word.is_empty() {
                cx.next_word
            } else {
                Some(NextWord { word, alone_on_line: is_first && is_last && cx.ends_line })
            };
            let cx = TextContext { next_word: next, prev_word, ..cx };
            // A line break right after an unescaped `\` would be a hard break
            // (the text's last word gets its `\` escaped instead, and may break)
            if prev_word.is_some_and(ends_with_unescaped_backslash) {
                parts.push_str(" ");
            } else {
                push_whitespace(ws.contains('\n'), &cx, parts, f);
            }
        }
        if word.is_empty() {
            break;
        }
        prev_word = Some(word);
        let printed: Cow<'a, str> = if in_delimiter {
            let prev = if is_first { cx.edge_prev } else { Some(' ') };
            let next = if is_last { cx.edge_next } else { Some(' ') };
            // A leading marker character would merge with the opening marker (`**foo*`)
            let escape_leading =
                is_first && !leading_ws && cx.first_of_delimiter == Some(word.as_bytes()[0]);
            print_delimited_word(word, escape_leading, prev, next)
        } else if prose_wrap == ProseWrap::Preserve
            && is_first
            && is_last
            && !leading_ws
            && cx.after_soft_break
            && is_fake_setext_underline(word)
        {
            Cow::Owned(format!("\\{word}"))
        } else {
            Cow::Borrowed(word)
        };
        // A line ending with `\` is a hard break (the source had whitespace after it, which goes);
        // a `\` right before the closing marker would escape the marker's first character
        let printed: Cow<'a, str> = if is_last
            && (cx.before_soft_break || cx.last_of_delimiter.is_some())
            && ends_with_unescaped_backslash(&printed)
        {
            Cow::Owned(format!("{printed}\\"))
        } else {
            printed
        };
        parts.push_str(arena_cow_str(&printed, f));
        rest = tail;
        is_first = false;
    }
}

/// The whitespace (a space run, or one holding a `newline`) between `cx.prev_word`
/// and `cx.next_word` (`None`: the whitespace touches a node edge):
/// a separator that may break, a space, or nothing.
pub fn push_whitespace<'a>(
    newline: bool,
    cx: &TextContext<'a>,
    parts: &mut Parts<'a>,
    f: &MarkdownFormatter<'_, 'a>,
) {
    let options = f.options();
    // A space only matters where it may become a break (`always`)
    let prose_wrap = if !cx.is_link
        && (newline || options.prose_wrap == ProseWrap::Always)
        && prevents_break(newline, cx.next_word, options.prose_wrap)
    {
        ProseWrap::Never
    } else {
        options.prose_wrap
    };

    if prose_wrap == ProseWrap::Preserve && newline {
        parts.push_sep(Sep::HardLine);
        return;
    }
    // The CJK rules only apply to a sentence with CJK text
    let (prev, next) = if cx.cj_spaces.is_some() {
        (
            cx.prev_word.and_then(|w| cjk::edges(w).map(|(_, last)| last)),
            cx.next_word.and_then(|w| cjk::edges(w.word).map(|(first, _)| first)),
        )
    } else {
        (None, None)
    };
    let can_be_space = !newline || line_break_can_be_space(prev, next, cx);
    let breakable = prose_wrap == ProseWrap::Always
        && f.context().no_wrap_depth().get() == 0
        && is_breakable(prev, next, cx);
    match (breakable, can_be_space) {
        (true, true) => parts.push_sep(Sep::Line),
        (true, false) => parts.push_sep(Sep::SoftLine),
        (false, true) => parts.push_str(" "),
        (false, false) => {}
    }
}

/// Prettier's `lineBreakCanBeConvertedToSpace`.
fn line_break_can_be_space(prev: Option<Edge>, next: Option<Edge>, cx: &TextContext<'_>) -> bool {
    if cx.is_link {
        return true;
    }
    let (Some(prev), Some(next)) = (prev, next) else { return true };
    let latin_like = |kind: Kind| matches!(kind, Kind::NonCjk | Kind::KLetter);
    // Between non-CJK / Korean words, or Korean and CJ, a line break is a space
    if (latin_like(prev.kind) && latin_like(next.kind))
        || matches!(
            (prev.kind, next.kind),
            (Kind::KLetter, Kind::CjLetter) | (Kind::CjLetter, Kind::KLetter)
        )
    {
        return true;
    }
    // A delimiter run glued to a CJ character gains flanking it did not have (`」\n**` → `」**` closes);
    // a space keeps it inert, as the line break did
    if is_delimiter_char(prev.ch) || is_delimiter_char(next.ch) {
        return true;
    }
    // Around CJK punctuation or between CJ letters it is nothing
    if prev.kind == Kind::CjkPunctuation
        || next.kind == Kind::CjkPunctuation
        || (prev.kind == Kind::CjLetter && next.kind == Kind::CjLetter)
    {
        return false;
    }
    // Between CJ and non-CJK: a space next to ASCII punctuation (`:::` fences),
    // nothing next to other punctuation (`〜`, `…`), else the sentence's own style.
    if prev.ch.is_ascii_punctuation() || next.ch.is_ascii_punctuation() {
        return true;
    }
    if prev.punctuation || next.punctuation {
        return false;
    }
    cx.cj_spaces.unwrap_or(false)
}

/// `*` `_` `~` `` ` ``: runs of these pair up by flanking or by length, so their neighbors matter.
fn is_delimiter_char(c: char) -> bool {
    matches!(c, '*' | '_' | '~' | '`')
}

/// Prettier's `isBreakable` for a `" "` / `"\n"` whitespace under `always`.
fn is_breakable(prev: Option<Edge>, next: Option<Edge>, cx: &TextContext<'_>) -> bool {
    if cx.is_link {
        return true;
    }
    let (Some(prev), Some(next)) = (prev, next) else { return true };
    // Korean next to CJ breaks;
    // anything else next to CJ never does (browsers turn the break into a space)
    matches!(
        (prev.kind, next.kind),
        (Kind::KLetter, Kind::CjLetter) | (Kind::CjLetter, Kind::KLetter)
    ) || !(prev.kind.is_cj() || next.kind.is_cj())
}

/// Prettier's `usesCJSpaces`, over the words of one sentence (the texts of a soft-break run):
/// whether spaces outnumber nothing between CJ and non-CJK words.
/// `words` yields each text's words with the whitespace before them (`None` for the first).
pub fn uses_cj_spaces<'a>(texts: impl Iterator<Item = &'a str>) -> bool {
    let (mut spaces, mut nothing) = (0u32, 0u32);
    let counts = |a: Kind, b: Kind| {
        matches!((a, b), (Kind::CjLetter, Kind::NonCjk) | (Kind::NonCjk, Kind::CjLetter))
    };
    for text in texts {
        let mut prev_last: Option<Kind> = None;
        let mut rest = text;
        loop {
            let after_ws = rest.trim_start_matches(is_split_whitespace);
            let ws = &rest[..rest.len() - after_ws.len()];
            rest = after_ws;
            let word_end = rest.find(is_split_whitespace).unwrap_or(rest.len());
            let (word, tail) = rest.split_at(word_end);
            if word.is_empty() {
                break;
            }
            let mut kinds = cjk::kinds(word);
            let first = kinds.next().unwrap_or(Kind::NonCjk);
            if let Some(prev) = prev_last
                && !ws.is_empty()
                && !ws.contains('\n')
                && counts(prev, first)
            {
                spaces += 1;
            }
            let mut last = first;
            for kind in kinds {
                if counts(last, kind) {
                    nothing += 1;
                }
                last = kind;
            }
            prev_last = Some(last);
            rest = tail;
        }
    }
    spaces > nothing
}

/// Never break before a word that would start a block.
pub fn prevents_break(newline: bool, next: Option<NextWord<'_>>, prose_wrap: ProseWrap) -> bool {
    let Some(next) = next else { return false };
    // Under `preserve` lines stay where they are, so whether the word stands alone is known
    if !looks_like_block_start(next, prose_wrap == ProseWrap::Preserve) {
        return false;
    }
    // A lone `---` / `===` after a newline is going to be escaped as a fake setext underline instead
    !(prose_wrap == ProseWrap::Preserve
        && newline
        && is_fake_setext_underline(next.word)
        && next.alone_on_line)
}

/// The parser's line-start classification (`lexical::line_start`), asked about the word alone:
/// list markers, `#`s, `>`, thematic breaks and setext underlines, fences, HTML and directive openers,
/// table rows, footnote definitions.
/// (Prettier tests `/^>|^(?:[*+-]|#{1,6}|\d+[).])$/` only, which is how a wrapped `<div>` or `***` opens a block.)
fn looks_like_block_start(next: NextWord<'_>, exact: bool) -> bool {
    // Asked about the line the wrap would produce:
    // the word alone (`***` is a break, `-|-` a delimiter row)
    // or the word with content after it (`- x` interrupts a paragraph where an empty `-` does not).
    // `exact`: the word's position is known; otherwise it depends on the wrapping itself, so both count.
    if !may_open_block(next.word.as_bytes()[0]) {
        return false;
    }
    let constructs = Constructs::markdown();
    if (!exact || next.alone_on_line) && lexical::line_start(&constructs, next.word, true).is_some()
    {
        return true;
    }
    if exact && next.alone_on_line {
        return false;
    }
    // A dialect line shape at a line start is printed raw from then on: never create one
    // (asked about the word alone, so a `:::` word counts even where `::: note` would be an opener)
    if is_line_shape_start(next.word) {
        return true;
    }
    let mut buf = [0u8; 64];
    let len = next.word.len();
    if len + 2 > buf.len() {
        return false;
    }
    buf[..len].copy_from_slice(next.word.as_bytes());
    buf[len..len + 2].copy_from_slice(b" x");
    let line = std::str::from_utf8(&buf[..len + 2]).unwrap_or(next.word);
    lexical::line_start(&constructs, line, true).is_some()
}

/// Every block start (and line shape) begins with one of these bytes.
pub fn may_open_block(first: u8) -> bool {
    matches!(
        first,
        b'>' | b'=' | b'-' | b'*' | b'_' | b'+' | b'0'
            ..=b'9' | b'#' | b'`' | b'~' | b'<' | b'$' | b'{' | b':' | b'[' | b'|'
    )
}

/// Dialect line shapes (see AGENTS.md "Dialects") that a line may start with:
/// a `:::` run that is not a directive opener (a bare closer, `:::)`), a VitePress `<<<` snippet import,
/// a component tag (`<Badge />`, `<my-element>`: uppercase or hyphenated names, which are not HTML).
pub fn is_line_shape_start(line: &str) -> bool {
    (line.starts_with(":::") && !is_directive_opener(line))
        || line.starts_with("<<<")
        || is_component_tag(line)
}

/// The parser's opener rule (`scan::directive_fence_open`): colons, optional spaces, a name-like start.
pub fn is_directive_opener(line: &str) -> bool {
    line.trim_start_matches(':')
        .trim_start_matches([' ', '\t'])
        .starts_with(|c: char| c.is_alphanumeric() || matches!(c, '_' | '{' | '['))
}

fn is_component_tag(raw: &str) -> bool {
    let Some(rest) = raw.strip_prefix('<') else { return false };
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let name_len = rest.bytes().take_while(|b| b.is_ascii_alphanumeric() || *b == b'-').count();
    let name = &rest[..name_len];
    name.starts_with(|c: char| c.is_ascii_uppercase())
        || (name.contains('-') && name.starts_with(|c: char| c.is_ascii_alphabetic()))
}

/// An odd run of `\` ends the word.
pub fn ends_with_unescaped_backslash(word: &str) -> bool {
    word.bytes().rev().take_while(|&b| b == b'\\').count() % 2 == 1
}

/// `^(?:=+|-+)$`
fn is_fake_setext_underline(word: &str) -> bool {
    !word.is_empty() && (word.bytes().all(|b| b == b'=') || word.bytes().all(|b| b == b'-'))
}

/// A word inside emphasis / strong, with the runs that could end it escaped.
///
/// `prev` / `next` are the characters around the word within its text;
/// `None` at the text edges, where neighbours across nodes are not consulted (as in Prettier) and nothing is escaped.
fn print_delimited_word<'a>(
    word: &'a str,
    escape_leading: bool,
    prev: Option<char>,
    next: Option<char>,
) -> Cow<'a, str> {
    if !word.contains(['*', '_']) {
        return Cow::Borrowed(word);
    }
    let text: Cow<'a, str> =
        if escape_leading { Cow::Owned(format!("\\{word}")) } else { Cow::Borrowed(word) };
    escape_delimiter_runs(text, prev, next)
}

/// Escapes every `*` / `_` run that could open or close emphasis where it stands, character by character
/// (a single backslash only shortens the run, and the rest can still pair: Prettier's `\***`).
///
/// One rule for every run
/// (Prettier's regex scan skips a run right after another run's following character; that gap is not reproduced).
fn escape_delimiter_runs(
    text: Cow<'_, str>,
    prev: Option<char>,
    next: Option<char>,
) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    let mut out: Option<String> = None;
    let mut emitted = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b'*' && b != b'_' {
            i += 1;
            continue;
        }
        // `*`, `_`, `\` are ASCII, so byte positions here are char boundaries
        let run_start = i;
        while i < bytes.len() && bytes[i] == b {
            i += 1;
        }
        // A backslash escapes one character: the rest of the run is a run of its own
        let backslashes = bytes[..run_start].iter().rev().take_while(|&&c| c == b'\\').count();
        let run_start = if backslashes % 2 == 1 { run_start + 1 } else { run_start };
        if run_start == i {
            continue;
        }
        let preceding = text[..run_start].chars().next_back().or(prev);
        let following = text[i..].chars().next().or(next);
        if can_open_or_close(preceding, b as char, following) == Some(true) {
            // Every character: escaping only the first leaves a shorter run that can still pair
            let out = out.get_or_insert_with(|| String::with_capacity(text.len() + 2));
            out.push_str(&text[emitted..run_start]);
            for _ in run_start..i {
                out.push('\\');
                out.push(b as char);
            }
            emitted = i;
        }
    }
    match out {
        None => text,
        Some(mut out) => {
            out.push_str(&text[emitted..]);
            Cow::Owned(out)
        }
    }
}

/// Whether a run of `indicator` could open or close emphasis here,
/// by the parser's own rule (micromark's flanking, marker-adjacency relaxation included);
/// `None` when a side is untouchable (an autolink, whose extent an escape would change).
fn can_open_or_close(
    preceding: Option<char>,
    indicator: char,
    following: Option<char>,
) -> Option<bool> {
    let (preceding, following) = (preceding?, following?);
    let (open, close) = attention(indicator as u8, Some(preceding), Some(following), true);
    Some(open || close)
}
