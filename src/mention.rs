//! Mentions: who a note is for, written in it (docs/protocol.md, "Mentions"). This reads them
//! from the text, as far as that can go without the database: where they are, what they say, and
//! the checks on their form. Who they name is `api`'s to find out.

use crate::db::{CONTEXT_FORMAT, RESERVED_HANDLES, valid_context};

/// At most this many distinct people and teams in one note.
pub const MAX_MENTIONS: usize = 20;

/// One mention: a name in lowercase (`team` for `@team` and `@equipo`), and the context it was
/// written with, if any.
#[derive(Debug, PartialEq, Clone)]
pub struct Mention {
    pub name: String,
    pub context: Option<String>,
}

/// The distinct mentions of `text`, in the order they first appear, without `me` (the writer:
/// mentioning yourself is ignored). Fails, with the message for `BAD_REQUEST`, when the same name
/// comes with two different contexts (or once with one and once without), when a context is
/// malformed or on `@team`, or when there are more than `MAX_MENTIONS`.
pub fn mentions(text: &str, me: &str) -> Result<Vec<Mention>, String> {
    let mut out: Vec<Mention> = Vec::new();
    for found in scan(text) {
        if found.name == me {
            continue;
        }
        if let Some(context) = &found.context {
            if found.name == "team" {
                return Err(format!(
                    "'@{}/{context}': @team takes no context",
                    found.written
                ));
            }
            if !valid_context(context) {
                return Err(format!(
                    "'@{}/{context}': a context must be {CONTEXT_FORMAT}",
                    found.written
                ));
            }
        }
        match out.iter().find(|m| m.name == found.name) {
            Some(seen) if seen.context == found.context => {}
            Some(_) => {
                return Err(format!(
                    "@{} is mentioned twice with different contexts (or once with one and once \
                     without): mention them once",
                    found.name
                ));
            }
            None => out.push(Mention {
                name: found.name,
                context: found.context,
            }),
        }
    }
    if out.len() > MAX_MENTIONS {
        return Err(format!(
            "at most {MAX_MENTIONS} people and teams can be mentioned in one note; this one \
             mentions {}",
            out.len()
        ));
    }
    Ok(out)
}

/// Whether `text` mentions someone, and only `me` (the writer): such a note names nobody, and yet
/// it is no note without mentions, the only kind that goes to the global board.
pub fn mentions_only(text: &str, me: &str) -> bool {
    let found = scan(text);
    !found.is_empty() && found.iter().all(|f| f.name == me)
}

/// A mention as found in the text, before any check.
#[derive(Debug, PartialEq)]
struct Found {
    /// The name, lowercased, `equipo` read as `team`.
    name: String,
    /// As written, for messages.
    written: String,
    /// What follows `/`, lowercased; `None` without one (or when nothing a context can hold
    /// follows it).
    context: Option<String>,
}

/// Every mention outside code, in order, repeats included.
fn scan(text: &str) -> Vec<Found> {
    let chars: Vec<char> = text.chars().collect();
    let code = code_mask(&chars);
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '@' || code[i] || (i > 0 && !may_precede(chars[i - 1])) {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while end < chars.len() && !code[end] && is_name_char(chars[end]) {
            end += 1;
        }
        // Trailing `-` is punctuation, not part of the name.
        let mut name_end = end;
        while name_end > start && chars[name_end - 1] == '-' {
            name_end -= 1;
        }
        let name: String = chars[start..name_end].iter().collect();
        if !is_name(&name) {
            i += 1;
            continue;
        }
        let mut context = None;
        let mut next = name_end;
        if name_end == end && end < chars.len() && chars[end] == '/' && !code[end] {
            let from = end + 1;
            let mut to = from;
            while to < chars.len() && !code[to] && is_context_char(chars[to]) {
                to += 1;
            }
            let mut last = to;
            while last > from && is_trailing(chars[last - 1]) {
                last -= 1;
            }
            if last > from {
                context = Some(chars[from..last].iter().collect::<String>().to_lowercase());
            }
            next = last.max(end);
        }
        let lower = name.to_lowercase();
        out.push(Found {
            name: if RESERVED_HANDLES.contains(&lower.as_str()) {
                "team".to_string()
            } else {
                lower
            },
            written: name,
            context,
        });
        i = next.max(i + 1);
    }
    out
}

/// A character after which an `@` may start a mention: not a letter or digit (of any script),
/// nor `.`, `_`, `-`, `+`, `/`, `@` or a backtick. So `pedro@empresa.com`,
/// `https://x.com/@ana` and `@@ana` mention nobody.
fn may_precede(c: char) -> bool {
    !(c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '/' | '@' | '`'))
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// `[A-Za-z0-9][A-Za-z0-9_-]{1,31}`.
fn is_name(name: &str) -> bool {
    (2..=32).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name.chars().all(is_name_char)
}

/// What a context can hold, in any case: `[a-z0-9._-]`, `@` before the branch, `/` in it.
fn is_context_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '@')
}

/// Punctuation that ends a mention rather than belonging to it.
fn is_trailing(c: char) -> bool {
    matches!(
        c,
        '.' | ',' | ';' | ':' | '!' | '?' | ')' | '-' | '/' | '\''
    )
}

/// Which characters of the text are code: in a fenced code block (from a line that starts, after
/// up to three spaces, with three or more backticks or tildes, to the line that closes it with at
/// least as many of the same and nothing else, or the end of the note) or in an inline code span
/// (between two runs of the same number of backticks, on one line or several of the same paragraph:
/// a span does not cross a blank line). A run of backticks that nothing closes is plain text.
fn code_mask(chars: &[char]) -> Vec<bool> {
    let mut mask = vec![false; chars.len()];
    // Lines, as [start, end) without the line break; a `\r` before it is left in.
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, c) in chars.iter().enumerate() {
        if *c == '\n' {
            lines.push((start, i));
            start = i + 1;
        }
    }
    lines.push((start, chars.len()));

    let mut open: Option<(char, usize)> = None;
    // The text outside fenced blocks, as ranges of whole lines, for the inline spans.
    let mut prose: Vec<(usize, usize)> = Vec::new();
    let mut prose_start: Option<usize> = None;
    for &(from, to) in &lines {
        let line = &chars[from..to];
        match open {
            Some((fence, len)) => {
                // Up to the end of the closing line (and its line break) is code.
                let end = (to + 1).min(chars.len());
                mask[from..end].iter_mut().for_each(|m| *m = true);
                if closes(line, fence, len) {
                    open = None;
                }
            }
            None => {
                if let Some(fence) = opens(line) {
                    if let Some(s) = prose_start.take() {
                        prose.push((s, from));
                    }
                    let end = (to + 1).min(chars.len());
                    mask[from..end].iter_mut().for_each(|m| *m = true);
                    open = Some(fence);
                } else if blank(line) {
                    // A blank line ends a paragraph, and no inline span crosses it.
                    if let Some(s) = prose_start.take() {
                        prose.push((s, from));
                    }
                } else if prose_start.is_none() {
                    prose_start = Some(from);
                }
            }
        }
    }
    if let Some(s) = prose_start {
        prose.push((s, chars.len()));
    }
    for (from, to) in prose {
        spans(&chars[from..to], &mut mask[from..to]);
    }
    mask
}

/// Whether a line is blank: empty, or only spaces, tabs (or the `\r` of a CRLF).
fn blank(line: &[char]) -> bool {
    line.iter().all(|c| matches!(c, ' ' | '\t' | '\r'))
}

/// Up to three spaces of indentation: the rest of the line, or `None` with more.
fn unindent(line: &[char]) -> Option<&[char]> {
    let spaces = line.iter().take_while(|c| **c == ' ').count();
    (spaces <= 3).then(|| &line[spaces..])
}

/// The fence a line opens, as (character, length): three or more backticks or tildes; after
/// backticks, no other backtick on the line (that is an inline span).
fn opens(line: &[char]) -> Option<(char, usize)> {
    let rest = unindent(line)?;
    let fence = *rest.first().filter(|c| matches!(c, '`' | '~'))?;
    let len = rest.iter().take_while(|c| **c == fence).count();
    if len < 3 || (fence == '`' && rest[len..].contains(&'`')) {
        return None;
    }
    Some((fence, len))
}

/// Whether a line closes a fence of `len` `fence` characters: at least as many, then only
/// spaces, tabs (or the `\r` of a CRLF).
fn closes(line: &[char], fence: char, len: usize) -> bool {
    let Some(rest) = unindent(line) else {
        return false;
    };
    let run = rest.iter().take_while(|c| **c == fence).count();
    run >= len && rest[run..].iter().all(|c| matches!(c, ' ' | '\t' | '\r'))
}

/// Marks the inline code spans of `text`: a run of n backticks opens one that the next run of
/// exactly n closes; a run nothing closes is plain text.
fn spans(text: &[char], mask: &mut [bool]) {
    let run_at = |i: usize| text[i..].iter().take_while(|c| **c == '`').count();
    let mut i = 0;
    while i < text.len() {
        if text[i] != '`' {
            i += 1;
            continue;
        }
        let n = run_at(i);
        let mut j = i + n;
        let mut close = None;
        while j < text.len() {
            if text[j] == '`' {
                let m = run_at(j);
                if m == n {
                    close = Some(j);
                    break;
                }
                j += m;
            } else {
                j += 1;
            }
        }
        match close {
            Some(j) => {
                mask[i..j + n].iter_mut().for_each(|m| *m = true);
                i = j + n;
            }
            None => i += n,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names (with `/context`) mentioned in `text`, as found, repeats included.
    fn found(text: &str) -> Vec<String> {
        scan(text)
            .into_iter()
            .map(|f| match f.context {
                Some(c) => format!("{}/{c}", f.name),
                None => f.name,
            })
            .collect()
    }

    #[test]
    fn where_a_mention_may_be() {
        let cases: &[(&str, &[&str])] = &[
            ("@ana", &["ana"]),
            ("hi @ana and @bob", &["ana", "bob"]),
            ("@Ana", &["ana"]),
            ("@ANA_B-2", &["ana_b-2"]),
            ("(@ana)", &["ana"]),
            ("[@ana]", &["ana"]),
            ("\"@ana\"", &["ana"]),
            ("*@ana*", &["ana"]),
            ("cc:@ana", &["ana"]),
            ("¡@ana!", &["ana"]),
            ("—@ana", &["ana"]),
            ("line\n@ana", &["ana"]),
            ("line\r\n@ana\r\n", &["ana"]),
            ("\t@ana", &["ana"]),
            // After a letter or digit (any script), `.`, `_`, `-`, `+`, `/`, `@` or a backtick:
            // nobody.
            ("pedro@empresa.com", &[]),
            ("ana@ana.com", &[]),
            ("x.@ana", &[]),
            ("x_@ana", &[]),
            ("x-@ana", &[]),
            ("x+@ana", &[]),
            ("https://x.com/@ana", &[]),
            ("@@ana", &[]),
            ("é@ana", &[]),
            ("日本@ana", &[]),
            ("ß@ana", &[]),
            ("٣@ana", &[]),
            ("1@ana", &[]),
            // Not a name: plain text.
            ("@ ana", &[]),
            ("@1", &[]),
            ("@a", &[]),
            ("@a b", &[]),
            ("@", &[]),
            ("x @", &[]),
            ("@-ana", &[]),
            ("@_ana", &[]),
            ("@ñandu", &[]),
            ("@é", &[]),
            ("email me: ana@example.org, or @bob", &["bob"]),
            ("npm i @types/node", &["types/node"]),
            ("@12", &["12"]),
        ];
        for (text, expected) in cases {
            assert_eq!(found(text), *expected, "{text:?}");
        }
        // A name is 2 to 32 characters.
        assert_eq!(found(&format!("@{}", "a".repeat(32))), ["a".repeat(32)]);
        assert!(found(&format!("@{}", "a".repeat(33))).is_empty());
    }

    #[test]
    fn what_ends_a_mention() {
        let cases: &[(&str, &[&str])] = &[
            ("@ana.", &["ana"]),
            ("@ana,", &["ana"]),
            ("@ana;", &["ana"]),
            ("@ana:", &["ana"]),
            ("@ana!", &["ana"]),
            ("@ana?", &["ana"]),
            ("@ana)", &["ana"]),
            ("@ana-", &["ana"]),
            ("@ana--", &["ana"]),
            ("@ana/", &["ana"]),
            ("@ana'", &["ana"]),
            ("@ana's thing", &["ana"]),
            ("@ana...", &["ana"]),
            ("@ana.b", &["ana"]),
            ("@ana@x", &["ana"]),
            ("@ana_", &["ana_"]),
            // A context, read up to what it cannot hold, its trailing punctuation left out.
            ("@ana/pagos-api", &["ana/pagos-api"]),
            ("@ana/Pagos-API.", &["ana/pagos-api"]),
            ("(@ana/pagos-api)", &["ana/pagos-api"]),
            (
                "@ana/pagos-api@feat/refunds:",
                &["ana/pagos-api@feat/refunds"],
            ),
            (
                "@ana/pagos-api@feat/refunds/ please",
                &["ana/pagos-api@feat/refunds"],
            ),
            ("@ana/pagos-api, @bob", &["ana/pagos-api", "bob"]),
            ("@ana/x y", &["ana/x"]),
            ("@ana/ y", &["ana"]),
            ("@ana/.", &["ana"]),
            ("@ana-/x", &["ana"]),
            ("@ana/x`y`", &["ana/x"]),
            ("@team", &["team"]),
            ("@Equipo", &["team"]),
            ("@backend", &["backend"]),
        ];
        for (text, expected) in cases {
            assert_eq!(found(text), *expected, "{text:?}");
        }
    }

    #[test]
    fn code_holds_no_mentions() {
        let cases: &[(&str, &[&str])] = &[
            ("`@ana`", &[]),
            ("`@ana` @bob", &["bob"]),
            ("``@ana``", &[]),
            ("`` `@ana` ``", &[]),
            ("``@ana` @bob``", &[]),
            // A run that nothing closes is text; a backtick before `@` still stops it.
            ("`@ana", &[]),
            ("` @ana", &["ana"]),
            ("``x` @ana", &["ana"]),
            // A span closes only on a run of the same length.
            ("``x` @ana ``", &[]),
            ("`x`` @ana `", &[]),
            // Across lines.
            ("`a\n@ana\nb` @bob", &["bob"]),
            ("`a\r\n@ana\r\n` @bob", &["bob"]),
            // But not across a blank line: a stray backtick does not hide the next paragraph.
            ("`a\n\n@ana `b`", &["ana"]),
            (
                "Build broke in `main.rs\n\n@bob can you run `cargo test`?",
                &["bob"],
            ),
            ("`a\n \t\r\n@ana `b`", &["ana"]),
            ("`a\r\n\r\n@ana\r\n`", &["ana"]),
            ("`a\n\n`@ana`", &[]),
            ("```\n\n@ana\n```\n@bob", &["bob"]),
            ("a `x` @ana `y`", &["ana"]),
            // After a span's closing backtick: not a mention.
            ("`x`@ana", &[]),
            (
                "@decorator `@Component` and `@angular/core`",
                &["decorator"],
            ),
            // Fenced blocks, with ` or ~, up to three spaces in.
            ("```\n@ana\n```\n@bob", &["bob"]),
            ("~~~\n@ana\n~~~\n@bob", &["bob"]),
            ("```rust\n@ana\n```\n@bob", &["bob"]),
            ("   ```\n@ana\n   ```\n@bob", &["bob"]),
            ("    ```\n@ana\n```", &["ana"]),
            ("````\n```\n@ana\n````\n@bob", &["bob"]),
            ("```\n@ana\n~~~\n@carl\n```\n@bob", &["bob"]),
            ("~~~~\n@ana\n~~~\n@carl\n~~~~~\n@bob", &["bob"]),
            ("```\n@ana\n``` x\n@carl\n```\n@bob", &["bob"]),
            ("```\n@ana", &[]),
            ("@ana\n```\n@carl", &["ana"]),
            ("```\r\n@ana\r\n```\r\n@bob", &["bob"]),
            ("```\n`\n```\n@bob `x`", &["bob"]),
            // Backticks after the fence on its line: an inline span, not a fence.
            ("```@ana``` @bob", &["bob"]),
            ("``` x ` @ana\n@bob", &["ana", "bob"]),
            // Fewer than three: not a fence.
            ("``\n@ana\n``", &[]),
            ("~~\n@ana\n~~", &["ana"]),
        ];
        for (text, expected) in cases {
            assert_eq!(found(text), *expected, "{text:?}");
        }
    }

    #[test]
    fn repeats_you_contexts_and_the_limit() {
        let m = |name: &str, context: Option<&str>| Mention {
            name: name.into(),
            context: context.map(String::from),
        };
        assert_eq!(
            mentions("@ana @Ana @bob @ana. @team @equipo", "zoe").unwrap(),
            vec![m("ana", None), m("bob", None), m("team", None)]
        );
        // Yourself: ignored, with or without a context.
        assert_eq!(
            mentions("@zoe @zoe/x @ana", "zoe").unwrap(),
            vec![m("ana", None)]
        );
        assert_eq!(mentions("@zoe hi", "zoe").unwrap(), vec![]);
        // A note whose only mentions are the writer is told apart from one with none.
        assert!(mentions_only("@zoe hi", "zoe"));
        assert!(mentions_only("@Zoe/x and @zoe", "zoe"));
        assert!(!mentions_only("hi", "zoe"));
        assert!(!mentions_only("@zoe @ana", "zoe"));
        assert!(!mentions_only("`@zoe` hi", "zoe"));
        assert_eq!(
            mentions("@ana/pagos-api and @ana/Pagos-Api", "zoe").unwrap(),
            vec![m("ana", Some("pagos-api"))]
        );
        for bad in [
            "@ana/pagos-api @ana",
            "@ana @ana/pagos-api",
            "@ana/x @ana/y",
            "@team/pagos",
            "@equipo/x",
            "@ana/x@",
            "@ana/a@b@c",
            "@ana/.x",
            "@ana/x/y",
        ] {
            assert!(mentions(bad, "zoe").is_err(), "{bad}");
        }
        let twenty: String = (0..20).map(|i| format!("@p{i:02} ")).collect();
        assert_eq!(mentions(&twenty, "zoe").unwrap().len(), 20);
        assert!(mentions(&format!("{twenty} @zoe @p00"), "zoe").is_ok());
        let err = mentions(&format!("{twenty} @p20"), "zoe").unwrap_err();
        assert!(err.contains("at most 20"), "{err}");
        // Mentions in code count for nothing.
        assert!(mentions(&format!("{twenty} `@p20`"), "zoe").is_ok());
    }
}
