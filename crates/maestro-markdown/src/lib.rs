//! What an agent writes, read into a model any frontend can draw: Markdown blocks and styled
//! runs of text, TeX maths as a tree ([`math`]), and code split into classed tokens
//! ([`highlight`]).
//!
//! Nothing here knows how anything is drawn. The gpui frontend lays the model out with its
//! theme; a terminal frontend can map the same runs and tokens to styled spans, and draw maths
//! with [`Math::to_unicode`].
//!
//! ```
//! let document = maestro_markdown::parse("Some **bold** text and $x^2$.");
//! let maestro_markdown::Block::Paragraph(text) = &document.blocks[0] else { panic!() };
//! assert_eq!(text.plain(), "Some bold text and x².");
//! ```

pub mod highlight;
pub mod math;

use pulldown_cmark::{CodeBlockKind, Event, Options, Tag, TagEnd};

pub use highlight::{Token, TokenClass, highlight};
pub use math::Math;

/// A whole message, as blocks from top to bottom.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    /// The blocks in order.
    pub blocks: Vec<Block>,
}

/// One block of a message.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    /// Running text.
    Paragraph(Text),
    /// A heading; `level` runs from 1 (the largest) to 6.
    Heading {
        /// How large: 1 to 6.
        level: u8,
        /// Its words.
        text: Text,
    },
    /// Fenced or indented code.
    Code(CodeBlock),
    /// Display maths, from `$$…$$` or `\[…\]`.
    Math(MathBlock),
    /// A quotation, which holds blocks of its own.
    Quote(Vec<Block>),
    /// A bulleted, numbered or task list.
    List(List),
    /// A table.
    Table(Table),
    /// A thematic break: `---`.
    Rule,
}

/// A block of code, already highlighted.
#[derive(Clone, Debug, PartialEq)]
pub struct CodeBlock {
    /// The language the fence names, if any: `rust`, `sh`.
    pub language: Option<String>,
    /// The code as written, without the fence.
    pub code: String,
    /// The code split into lines of tokens.
    pub lines: Vec<Vec<Token>>,
}

/// Display maths.
#[derive(Clone, Debug, PartialEq)]
pub struct MathBlock {
    /// The TeX as written.
    pub source: String,
    /// The TeX, read.
    pub math: Math,
}

/// A list.
#[derive(Clone, Debug, PartialEq)]
pub struct List {
    /// The first number of a numbered list; `None` for bullets.
    pub start: Option<u64>,
    /// The items in order.
    pub items: Vec<Item>,
}

/// One item of a [`List`].
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// For a task list item, whether it is ticked.
    pub checked: Option<bool>,
    /// What the item holds, including any nested lists.
    pub blocks: Vec<Block>,
}

/// A table.
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    /// How each column's cells are aligned.
    pub alignments: Vec<Align>,
    /// The header row.
    pub header: Vec<Text>,
    /// The body rows.
    pub rows: Vec<Vec<Text>>,
}

/// How a table column is aligned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    /// As the frontend likes: leading.
    None,
    /// Leading.
    Left,
    /// Centred.
    Center,
    /// Trailing.
    Right,
}

/// Styled text inside a block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Text {
    /// Runs of one style each, in order.
    pub runs: Vec<Run>,
}

/// A run of text in one style.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    /// The text. Inline maths is here as [`Math::to_unicode`]; a hard line break is `\n`.
    pub text: String,
    /// How it is set.
    pub style: Style,
}

/// How a [`Run`] is set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Style {
    /// `**strong**`.
    pub strong: bool,
    /// `*emphasis*`.
    pub emphasis: bool,
    /// `~~struck through~~`.
    pub strikethrough: bool,
    /// `` `code` ``.
    pub code: bool,
    /// Inline maths, `$…$`.
    pub math: bool,
    /// Where a link goes.
    pub link: Option<String>,
}

impl Text {
    /// The text without its styles.
    pub fn plain(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }

    /// Whether there is no text at all.
    pub fn is_empty(&self) -> bool {
        self.runs.iter().all(|run| run.text.is_empty())
    }

    fn push(&mut self, text: &str, style: &Style) {
        if text.is_empty() {
            return;
        }
        match self.runs.last_mut() {
            Some(last) if last.style == *style => last.text.push_str(text),
            _ => self.runs.push(Run { text: text.to_owned(), style: style.clone() }),
        }
    }

    /// Drop the spaces a line break leaves at either end.
    fn trim(mut self) -> Self {
        if let Some(first) = self.runs.first_mut() {
            first.text = first.text.trim_start().to_owned();
        }
        if let Some(last) = self.runs.last_mut() {
            last.text = last.text.trim_end().to_owned();
        }
        self.runs.retain(|run| !run.text.is_empty());
        self
    }
}

impl Document {
    /// The document as it reads without styles, one block per paragraph: what copying the
    /// whole message as plain text would give.
    pub fn plain(&self) -> String {
        fn write(blocks: &[Block], out: &mut Vec<String>) {
            for block in blocks {
                match block {
                    Block::Paragraph(text) | Block::Heading { text, .. } => out.push(text.plain()),
                    Block::Code(code) => out.push(code.code.clone()),
                    Block::Math(math) => out.push(math.math.to_unicode()),
                    Block::Quote(blocks) => write(blocks, out),
                    Block::List(list) => {
                        for item in &list.items {
                            write(&item.blocks, out);
                        }
                    }
                    Block::Table(table) => {
                        for row in std::iter::once(&table.header).chain(&table.rows) {
                            let cells: Vec<String> = row.iter().map(Text::plain).collect();
                            out.push(cells.join("\t"));
                        }
                    }
                    Block::Rule => {}
                }
            }
        }
        let mut out = Vec::new();
        write(&self.blocks, &mut out);
        out.join("\n\n")
    }
}

/// Read `source` as Markdown with tables, strikethrough, task lists and TeX maths.
///
/// Maths may be written `$…$` and `$$…$$`, or `\(…\)` and `\[…\]` as many models do. Text
/// still arriving is fine: an unfinished construct reads as plain text until it closes.
pub fn parse(source: &str) -> Document {
    let source = tex_delimiters(source);
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM;
    let mut reader = Reader {
        events: pulldown_cmark::Parser::new_ext(&source, options).peekable(),
        style: Style::default(),
        strong: 0,
        emphasis: 0,
        strikethrough: 0,
        links: Vec::new(),
        task: None,
        depth: 0,
    };
    Document { blocks: reader.blocks() }
}

/// `\(…\)` and `\[…\]` rewritten as `$…$` and `$$…$$`, outside code.
fn tex_delimiters(source: &str) -> std::borrow::Cow<'_, str> {
    if !source.contains("\\(") && !source.contains("\\[") {
        return source.into();
    }
    let mut out = String::with_capacity(source.len());
    let mut fence: Option<String> = None;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker: String = trimmed.chars().take_while(|c| *c == '`' || *c == '~').collect();
        if let Some(open) = &fence {
            if marker.len() >= open.len() && marker.starts_with(&open[..1]) {
                fence = None;
            }
            out.push_str(line);
            continue;
        }
        if marker.len() >= 3
            && (marker.chars().all(|c| c == '`') || marker.chars().all(|c| c == '~'))
        {
            fence = Some(marker);
            out.push_str(line);
            continue;
        }
        // Outside code spans, swap the delimiters.
        let mut ticks = 0;
        let mut chars = line.char_indices().peekable();
        while let Some((_, c)) = chars.next() {
            if c == '`' {
                let mut run = 1;
                while chars.peek().is_some_and(|(_, c)| *c == '`') {
                    chars.next();
                    run += 1;
                }
                ticks = if ticks == 0 {
                    run
                } else if ticks == run {
                    0
                } else {
                    ticks
                };
                out.extend(std::iter::repeat_n('`', run));
                continue;
            }
            if c == '\\' && ticks == 0 {
                match chars.peek().map(|(_, c)| *c) {
                    Some('(') | Some(')') => {
                        chars.next();
                        out.push('$');
                        continue;
                    }
                    Some('[') | Some(']') => {
                        chars.next();
                        out.push_str("$$");
                        continue;
                    }
                    Some('\\') => {
                        // An escaped backslash is never a delimiter.
                        chars.next();
                        out.push_str("\\\\");
                        continue;
                    }
                    _ => {}
                }
            }
            out.push(c);
        }
    }
    out.into()
}

struct Reader<'a> {
    events: std::iter::Peekable<pulldown_cmark::Parser<'a>>,
    style: Style,
    strong: usize,
    emphasis: usize,
    strikethrough: usize,
    links: Vec<String>,
    /// The tick of the task list item being read.
    task: Option<bool>,
    /// How many containers deep the reader is. Reading blocks is recursive, and the Markdown
    /// comes from an agent's answer or the user's own message, neither of which is trusted to
    /// be shallow.
    depth: usize,
}

/// How many containers deep blocks are read. Past it a container's content is dropped rather
/// than read: nothing a person writes nests nearly this far, and something that does must not
/// overflow the stack.
const MAX_DEPTH: usize = 48;

/// A piece of a paragraph: text, or display maths that breaks it.
enum Piece {
    Text(Text),
    Math(String),
}

fn is_inline(event: &Event<'_>) -> bool {
    match event {
        Event::Start(tag) => matches!(
            tag,
            Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. }
        ),
        Event::End(tag) => matches!(
            tag,
            TagEnd::Emphasis
                | TagEnd::Strong
                | TagEnd::Strikethrough
                | TagEnd::Link
                | TagEnd::Image
        ),
        Event::Text(_)
        | Event::Code(_)
        | Event::InlineMath(_)
        | Event::DisplayMath(_)
        | Event::InlineHtml(_)
        | Event::Html(_)
        | Event::SoftBreak
        | Event::HardBreak
        | Event::FootnoteReference(_)
        | Event::TaskListMarker(_) => true,
        _ => false,
    }
}

impl Reader<'_> {
    /// Blocks up to the end of the container being read, whose end it consumes.
    fn blocks(&mut self) -> Vec<Block> {
        if self.depth >= MAX_DEPTH {
            self.skip();
            return Vec::new();
        }
        self.depth += 1;
        let blocks = self.read_blocks();
        self.depth -= 1;
        blocks
    }

    /// Consumes the events up to the end of the container being read, keeping none of them.
    fn skip(&mut self) {
        let mut depth = 1usize;
        for event in self.events.by_ref() {
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    fn read_blocks(&mut self) -> Vec<Block> {
        let mut blocks = Vec::new();
        while let Some(event) = self.events.peek() {
            if is_inline(event) {
                // A tight list item's text comes without a paragraph around it.
                let pieces = self.inline(false);
                push_pieces(&mut blocks, pieces, Block::Paragraph);
                continue;
            }
            let Some(event) = self.events.next() else { break };
            match event {
                Event::Start(Tag::Paragraph) => {
                    let pieces = self.inline(true);
                    push_pieces(&mut blocks, pieces, Block::Paragraph);
                }
                Event::Start(Tag::Heading { level, .. }) => {
                    let level = level as u8;
                    let pieces = self.inline(true);
                    push_pieces(&mut blocks, pieces, |text| Block::Heading { level, text });
                }
                Event::Start(Tag::BlockQuote(_)) => blocks.push(Block::Quote(self.blocks())),
                Event::Start(Tag::CodeBlock(kind)) => {
                    let language = match kind {
                        CodeBlockKind::Fenced(info) => info
                            .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
                            .next()
                            .filter(|word| !word.is_empty())
                            .map(str::to_owned),
                        CodeBlockKind::Indented => None,
                    };
                    let mut code = String::new();
                    for event in self.events.by_ref() {
                        match event {
                            Event::Text(text) => code.push_str(&text),
                            Event::End(TagEnd::CodeBlock) => break,
                            _ => {}
                        }
                    }
                    if code.ends_with('\n') {
                        code.pop();
                    }
                    let lines = highlight(&code, language.as_deref());
                    blocks.push(Block::Code(CodeBlock { language, code, lines }));
                }
                Event::Start(Tag::List(start)) => {
                    let mut items = Vec::new();
                    while let Some(event) = self.events.next() {
                        match event {
                            Event::Start(Tag::Item) => {
                                let outer = self.task.take();
                                let blocks = self.blocks();
                                items.push(Item { checked: self.task.take(), blocks });
                                self.task = outer;
                            }
                            Event::End(TagEnd::List(_)) => break,
                            _ => {}
                        }
                    }
                    blocks.push(Block::List(List { start, items }));
                }
                Event::Start(Tag::Table(alignments)) => blocks.push(Block::Table(
                    self.table(
                        alignments
                            .into_iter()
                            .map(|align| match align {
                                pulldown_cmark::Alignment::None => Align::None,
                                pulldown_cmark::Alignment::Left => Align::Left,
                                pulldown_cmark::Alignment::Center => Align::Center,
                                pulldown_cmark::Alignment::Right => Align::Right,
                            })
                            .collect(),
                    ),
                )),
                Event::Start(Tag::HtmlBlock) => {
                    let mut html = String::new();
                    for event in self.events.by_ref() {
                        match event {
                            Event::Html(text) | Event::Text(text) => html.push_str(&text),
                            Event::End(TagEnd::HtmlBlock) => break,
                            _ => {}
                        }
                    }
                    let mut text = Text::default();
                    text.push(html.trim_end(), &Style::default());
                    if !text.is_empty() {
                        blocks.push(Block::Paragraph(text));
                    }
                }
                Event::Rule => blocks.push(Block::Rule),
                Event::Start(_) => blocks.extend(self.blocks()),
                Event::End(_) => break,
                _ => {}
            }
        }
        blocks
    }

    fn table(&mut self, alignments: Vec<Align>) -> Table {
        let mut table = Table { alignments, header: Vec::new(), rows: Vec::new() };
        let mut row = Vec::new();
        while let Some(event) = self.events.next() {
            match event {
                Event::Start(Tag::TableCell) => {
                    let mut cell = Text::default();
                    for piece in self.inline(true) {
                        match piece {
                            Piece::Text(text) => cell.runs.extend(text.runs),
                            Piece::Math(source) => cell.push(
                                &math::parse(&source).to_unicode(),
                                &Style { math: true, ..Style::default() },
                            ),
                        }
                    }
                    row.push(cell);
                }
                Event::End(TagEnd::TableHead) => table.header = std::mem::take(&mut row),
                Event::End(TagEnd::TableRow) => table.rows.push(std::mem::take(&mut row)),
                Event::End(TagEnd::Table) => break,
                _ => {}
            }
        }
        table
    }

    fn restyle(&mut self) {
        self.style.strong = self.strong > 0;
        self.style.emphasis = self.emphasis > 0;
        self.style.strikethrough = self.strikethrough > 0;
        self.style.link = self.links.last().cloned();
    }

    /// Inline content: up to and including the end of its block when `closed`, otherwise up
    /// to the next event that is not inline.
    fn inline(&mut self, closed: bool) -> Vec<Piece> {
        let mut pieces = Vec::new();
        let mut text = Text::default();
        loop {
            if !closed && !self.events.peek().is_some_and(is_inline) {
                break;
            }
            let Some(event) = self.events.next() else { break };
            match event {
                Event::Text(chunk) => text.push(&chunk, &self.style),
                Event::Code(code) => {
                    text.push(&code, &Style { code: true, ..self.style.clone() });
                }
                Event::InlineMath(source) => text.push(
                    &math::parse(&source).to_unicode(),
                    &Style { math: true, ..self.style.clone() },
                ),
                Event::DisplayMath(source) => {
                    pieces.push(Piece::Text(std::mem::take(&mut text)));
                    pieces.push(Piece::Math(source.trim().to_owned()));
                }
                Event::Html(html) | Event::InlineHtml(html) => {
                    // `<br>` is a line break; other tags show as written.
                    let tag = html.trim().to_ascii_lowercase();
                    if matches!(tag.as_str(), "<br>" | "<br/>" | "<br />") {
                        text.push("\n", &self.style);
                    } else {
                        text.push(&html, &self.style);
                    }
                }
                Event::SoftBreak => text.push(" ", &self.style),
                Event::HardBreak => text.push("\n", &self.style),
                Event::FootnoteReference(label) => text.push(&format!("[{label}]"), &self.style),
                Event::TaskListMarker(checked) => self.task = Some(checked),
                Event::Start(Tag::Emphasis) => {
                    self.emphasis += 1;
                    self.restyle();
                }
                Event::Start(Tag::Strong) => {
                    self.strong += 1;
                    self.restyle();
                }
                Event::Start(Tag::Strikethrough) => {
                    self.strikethrough += 1;
                    self.restyle();
                }
                Event::Start(Tag::Link { dest_url, .. })
                | Event::Start(Tag::Image { dest_url, .. }) => {
                    self.links.push(dest_url.into_string());
                    self.restyle();
                }
                Event::End(TagEnd::Emphasis) => {
                    self.emphasis = self.emphasis.saturating_sub(1);
                    self.restyle();
                }
                Event::End(TagEnd::Strong) => {
                    self.strong = self.strong.saturating_sub(1);
                    self.restyle();
                }
                Event::End(TagEnd::Strikethrough) => {
                    self.strikethrough = self.strikethrough.saturating_sub(1);
                    self.restyle();
                }
                Event::End(TagEnd::Link) | Event::End(TagEnd::Image) => {
                    self.links.pop();
                    self.restyle();
                }
                Event::End(_) => break,
                // Something block-level inside a paragraph: leave it.
                _ => {}
            }
        }
        pieces.push(Piece::Text(text));
        pieces
    }
}

/// Paragraph pieces as blocks: each run of text in `wrap`, each display formula its own
/// block.
fn push_pieces(blocks: &mut Vec<Block>, pieces: Vec<Piece>, wrap: impl Fn(Text) -> Block) {
    for piece in pieces {
        match piece {
            Piece::Text(text) => {
                let text = text.trim();
                if !text.is_empty() {
                    blocks.push(wrap(text));
                }
            }
            Piece::Math(source) => {
                let math = math::parse(&source);
                blocks.push(Block::Math(MathBlock { source, math }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paragraph(block: &Block) -> &Text {
        match block {
            Block::Paragraph(text) => text,
            other => panic!("not a paragraph: {other:?}"),
        }
    }

    #[test]
    fn inline_styles_become_runs() {
        let document = parse("Plain **bold *both*** `code` [link](https://x.dev) ~~gone~~");
        let text = paragraph(&document.blocks[0]);
        assert_eq!(text.plain(), "Plain bold both code link gone");
        let run = |needle: &str| text.runs.iter().find(|run| run.text == needle).unwrap();
        assert!(run("bold ").style.strong && !run("bold ").style.emphasis);
        assert!(run("both").style.strong && run("both").style.emphasis);
        assert!(run("code").style.code);
        assert_eq!(run("link").style.link.as_deref(), Some("https://x.dev"));
        assert!(run("gone").style.strikethrough);
    }

    #[test]
    fn headings_lists_and_quotes() {
        let document = parse("# Title\n\n- one\n- two\n  1. nested\n\n> quoted\n\n---");
        assert!(
            matches!(&document.blocks[0], Block::Heading { level: 1, text } if text.plain() == "Title")
        );
        let Block::List(list) = &document.blocks[1] else { panic!() };
        assert_eq!(list.start, None);
        assert_eq!(list.items.len(), 2);
        assert_eq!(paragraph(&list.items[0].blocks[0]).plain(), "one");
        let Block::List(nested) = &list.items[1].blocks[1] else { panic!() };
        assert_eq!(nested.start, Some(1));
        let Block::Quote(quote) = &document.blocks[2] else { panic!() };
        assert_eq!(paragraph(&quote[0]).plain(), "quoted");
        assert_eq!(document.blocks[3], Block::Rule);
    }

    #[test]
    fn task_lists_keep_their_ticks() {
        let document = parse("- [x] done\n- [ ] to do\n- plain");
        let Block::List(list) = &document.blocks[0] else { panic!() };
        let ticks: Vec<_> = list.items.iter().map(|item| item.checked).collect();
        assert_eq!(ticks, [Some(true), Some(false), None]);
    }

    #[test]
    fn code_blocks_are_highlighted() {
        let document = parse("```rust\nfn main() {}\n```\n\n    indented");
        let Block::Code(code) = &document.blocks[0] else { panic!() };
        assert_eq!(code.language.as_deref(), Some("rust"));
        assert_eq!(code.code, "fn main() {}");
        assert_eq!(code.lines[0][0], Token { text: "fn".into(), class: TokenClass::Keyword });
        let Block::Code(indented) = &document.blocks[1] else { panic!() };
        assert_eq!(indented.language, None);
        assert_eq!(indented.code, "indented");
    }

    #[test]
    fn maths_inline_and_on_its_own() {
        let document = parse("Energy is $E = mc^2$.\n\n$$\n\\frac{a}{b}\n$$");
        let text = paragraph(&document.blocks[0]);
        assert_eq!(text.plain(), "Energy is E = mc².");
        assert!(text.runs.iter().any(|run| run.style.math && run.text == "E = mc²"));
        let Block::Math(block) = &document.blocks[1] else { panic!() };
        assert_eq!(block.source, "\\frac{a}{b}");
        assert!(matches!(block.math, Math::Frac { .. }));
    }

    #[test]
    fn display_maths_breaks_its_paragraph() {
        let document = parse("Before $$x$$ after");
        assert_eq!(document.blocks.len(), 3);
        assert_eq!(paragraph(&document.blocks[0]).plain(), "Before");
        assert!(matches!(document.blocks[1], Block::Math(_)));
        assert_eq!(paragraph(&document.blocks[2]).plain(), "after");
    }

    #[test]
    fn latex_style_delimiters() {
        let document = parse("Inline \\(x^2\\) and\n\n\\[\n\\sqrt{y}\n\\]\n\n`\\(code\\)`");
        assert_eq!(paragraph(&document.blocks[0]).plain(), "Inline x² and");
        assert!(
            matches!(&document.blocks[1], Block::Math(block) if matches!(block.math, Math::Sqrt { .. }))
        );
        assert_eq!(paragraph(&document.blocks[2]).plain(), "\\(code\\)");
        // Code blocks are left alone.
        let document = parse("```\n\\[x\\]\n```");
        let Block::Code(code) = &document.blocks[0] else { panic!() };
        assert_eq!(code.code, "\\[x\\]");
    }

    #[test]
    fn dollar_amounts_are_not_maths() {
        let document = parse("It costs $5 and $10.");
        assert_eq!(paragraph(&document.blocks[0]).plain(), "It costs $5 and $10.");
    }

    #[test]
    fn tables() {
        let document = parse("| a | b |\n|:--|--:|\n| 1 | $x$ |\n| 2 | **3** |");
        let Block::Table(table) = &document.blocks[0] else { panic!() };
        assert_eq!(table.alignments, [Align::Left, Align::Right]);
        assert_eq!(table.header.iter().map(Text::plain).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(table.rows.len(), 2);
        assert!(table.rows[0][1].runs[0].style.math);
        assert!(table.rows[1][1].runs[0].style.strong);
    }

    #[test]
    fn breaks_inside_a_paragraph() {
        let document = parse("one\ntwo  \nthree");
        assert_eq!(paragraph(&document.blocks[0]).plain(), "one two\nthree");
    }

    #[test]
    fn half_written_text_still_reads() {
        for source in ["```rust\nfn", "$$\\frac{a", "**bold", "| a |\n|--", "- [", "\\["] {
            let _ = parse(source);
        }
        let document = parse("```rust\nfn ma");
        let Block::Code(code) = &document.blocks[0] else { panic!() };
        assert_eq!(code.code, "fn ma");
    }

    /// Reading blocks is recursive, so Markdown nested deeper than the stack can take used to
    /// abort the process rather than panic. Past [`MAX_DEPTH`] a container's content is
    /// dropped, and however deep the nesting goes the document still comes back.
    #[test]
    fn nesting_deeper_than_the_limit_is_dropped() {
        // Shallow nesting is still read, down to the innermost paragraph.
        let quoted = parse(&format!("{}deep\n", "> ".repeat(MAX_DEPTH - 1)));
        assert_eq!(quoted.plain(), "deep");

        for deep in [
            format!("{}deep\n", "> ".repeat(100_000)),
            (0..5_000).map(|i| format!("{}- x\n", "  ".repeat(i))).collect(),
            format!("${}x{}$", "{".repeat(100_000), "}".repeat(100_000)),
        ] {
            let _ = parse(&deep).plain();
        }
    }

    #[test]
    fn plain_text_of_a_document() {
        let document = parse("# A\n\nb *c*\n\n```\nd\n```");
        assert_eq!(document.plain(), "A\n\nb c\n\nd");
    }
}
