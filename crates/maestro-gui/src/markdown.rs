//! An agent's answer drawn as rich text: the Markdown, maths and highlighted code that
//! `maestro-markdown` reads, laid out with Atelier's tokens.
//!
//! Reading happens in `maestro-markdown`, which knows nothing of gpui, so a terminal frontend
//! can draw the same model. Only the drawing lives here: text runs, code blocks and tables in
//! the theme, and display maths typeset by RaTeX into an image.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use atelier::{
    ActiveTheme, CopyButton, CopySelection, Icon, IconName, SELECTABLE_CONTEXT, SelectAll, Theme,
};
use gpui::{
    AnyElement, App, ClipboardItem, ElementId, FocusHandle, FontStyle, FontWeight, Hsla,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Point, RenderOnce, SharedString, StatefulInteractiveElement,
    StrikethroughStyle, Styled, StyledText, TextLayout, TextRun, UnderlineStyle, Window, canvas,
    div, img, prelude::FluentBuilder, px, relative,
};
use maestro_markdown::{
    Align, Block, CodeBlock, Document, List, MathBlock, Run, Table, Text, TokenClass,
};

/// How many read messages are kept; a streaming reply reads anew with every delta, so the
/// cache is emptied when it grows past this rather than holding every version.
const CACHE_LIMIT: usize = 256;

thread_local! {
    static DOCUMENTS: RefCell<HashMap<u64, Rc<Document>>> = RefCell::new(HashMap::new());
    static FORMULAS: RefCell<HashMap<FormulaKey, Option<Rc<Formula>>>> =
        RefCell::new(HashMap::new());
}

/// `text` read as Markdown, from the cache when the same text was read before: a window
/// redraws often, and reading and highlighting every message each frame would be waste.
fn document(text: &str) -> Rc<Document> {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let key = hasher.finish();
    DOCUMENTS.with_borrow_mut(|cache| {
        if let Some(document) = cache.get(&key) {
            return document.clone();
        }
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        let document = Rc::new(maestro_markdown::parse(text));
        cache.insert(key, document.clone());
        document
    })
}

/// A reply drawn as rich text. Each block's text can be selected and copied.
#[derive(IntoElement)]
pub struct Markdown {
    id: ElementId,
    text: SharedString,
}

impl Markdown {
    /// `text` as Markdown. The id keeps each block's selection from frame to frame.
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self { id: id.into(), text: text.into() }
    }
}

impl RenderOnce for Markdown {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let document = document(&self.text);
        let theme = cx.theme().clone();
        let mut counter = 0;
        let mut painter = Painter { id: self.id, counter: &mut counter, theme: &theme, window };
        div()
            .flex()
            .flex_col()
            .gap(theme.spacing.md)
            .w_full()
            .min_w_0()
            .children(painter.blocks(&document.blocks))
    }
}

/// Draws blocks, numbering their ids in reading order.
struct Painter<'a> {
    id: ElementId,
    counter: &'a mut usize,
    theme: &'a Theme,
    window: &'a mut Window,
}

impl Painter<'_> {
    fn next_id(&mut self, kind: &str) -> ElementId {
        *self.counter += 1;
        ElementId::NamedChild(Box::new(self.id.clone()), format!("{kind}-{}", self.counter).into())
    }

    fn blocks(&mut self, blocks: &[Block]) -> Vec<AnyElement> {
        blocks.iter().map(|block| self.block(block)).collect()
    }

    fn block(&mut self, block: &Block) -> AnyElement {
        let theme = self.theme;
        match block {
            Block::Paragraph(text) => {
                RichText::from_text(self.next_id("p"), text, theme).into_any_element()
            }
            Block::Heading { level, text } => {
                let size = match level {
                    1 => theme.typography.size_xl,
                    2 => theme.typography.size_lg,
                    _ => theme.typography.size_md,
                };
                div()
                    .when(*level <= 2, |heading| heading.pt(theme.spacing.xs))
                    .child(
                        RichText::from_text(self.next_id("h"), text, theme)
                            .size(size)
                            .weight(theme.typography.heading_weight),
                    )
                    .into_any_element()
            }
            Block::Code(code) => self.code(code),
            Block::Math(math) => self.math(math),
            Block::Quote(blocks) => div()
                .flex()
                .flex_col()
                .gap(theme.spacing.sm)
                .pl(theme.spacing.md)
                .border_l_2()
                .border_color(theme.colors.border_strong)
                .text_color(theme.colors.text_muted)
                .children(self.blocks(blocks))
                .into_any_element(),
            Block::List(list) => self.list(list),
            Block::Table(table) => self.table(table),
            Block::Rule => div().h(px(1.)).bg(theme.colors.separator).into_any_element(),
        }
    }

    fn list(&mut self, list: &List) -> AnyElement {
        let theme = self.theme;
        let width = match list.start {
            // Wide enough for the longest number.
            Some(start) => {
                px(10. + 8. * (start + list.items.len() as u64).to_string().len() as f32)
            }
            None => px(14.),
        };
        let items: Vec<AnyElement> = list
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let marker = match (item.checked, list.start) {
                    (Some(checked), _) => task_box(checked, theme).into_any_element(),
                    (None, Some(start)) => div()
                        .text_color(theme.colors.text_muted)
                        .child(format!("{}.", start + index as u64))
                        .into_any_element(),
                    (None, None) => {
                        div().text_color(theme.colors.text_faint).child("•").into_any_element()
                    }
                };
                div()
                    .flex()
                    .gap(theme.spacing.xs)
                    .child(div().flex().flex_none().justify_end().w(width).child(marker))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(theme.spacing.xs)
                            .flex_1()
                            .min_w_0()
                            .children(self.blocks(&item.blocks)),
                    )
                    .into_any_element()
            })
            .collect();
        div().flex().flex_col().gap(theme.spacing.xs).children(items).into_any_element()
    }

    fn table(&mut self, table: &Table) -> AnyElement {
        let theme = self.theme;
        let columns = table.header.len().max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
        if columns == 0 {
            return div().into_any_element();
        }
        let mut cells = Vec::new();
        let rows =
            std::iter::once((true, &table.header)).chain(table.rows.iter().map(|r| (false, r)));
        let count = table.rows.len();
        for (index, (header, row)) in rows.enumerate() {
            let last = index == count;
            for column in 0..columns {
                let align = table.alignments.get(column).copied().unwrap_or(Align::None);
                let text = row.get(column).cloned().unwrap_or_default();
                let mut rich = RichText::from_text(self.next_id("cell"), &text, theme);
                if header {
                    rich = rich.weight(theme.typography.label_weight);
                }
                cells.push(
                    div()
                        .flex()
                        .min_w_0()
                        .px(theme.spacing.sm)
                        .py(theme.spacing.xs)
                        .when(!last, |cell| cell.border_b_1().border_color(theme.colors.border))
                        .when(header, |cell| cell.bg(theme.colors.sunken))
                        .map(|cell| match align {
                            Align::Center => cell.justify_center(),
                            Align::Right => cell.justify_end(),
                            Align::Left | Align::None => cell,
                        })
                        .child(rich)
                        .into_any_element(),
                );
            }
        }
        div()
            .id(self.next_id("table"))
            .overflow_x_scroll()
            .rounded(theme.shape.radius_md)
            .border_1()
            .border_color(theme.colors.border)
            .child(
                div()
                    .grid()
                    .grid_cols(columns as u16)
                    .min_w(px(120. * columns as f32))
                    .text_size(theme.typography.size_sm)
                    .children(cells),
            )
            .into_any_element()
    }

    /// A code block in the frame Atelier's `CodeBlock` uses, with the language and a copy
    /// button in its title bar and the code highlighted.
    fn code(&mut self, block: &CodeBlock) -> AnyElement {
        let theme = self.theme;
        let id = self.next_id("code");
        let language = block.language.clone().unwrap_or_else(|| "text".into());
        let mut text = String::new();
        let mut spans = Vec::new();
        for (index, line) in block.lines.iter().enumerate() {
            if index > 0 {
                text.push('\n');
            }
            for token in line {
                let start = text.len();
                text.push_str(&token.text);
                spans.push((start..text.len(), token_span(token.class, theme)));
            }
        }
        div()
            .flex()
            .flex_col()
            .rounded(theme.shape.radius_md)
            .bg(theme.colors.sunken)
            .border_1()
            .border_color(theme.colors.border)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(theme.spacing.sm)
                    .h(theme.density.control_md)
                    .pl(theme.spacing.md)
                    .pr(theme.spacing.xs)
                    .border_b_1()
                    .border_color(theme.colors.border)
                    .child(Icon::new(IconName::Code).size(px(13.)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font(theme.typography.mono.clone())
                            .text_size(theme.typography.size_sm)
                            .text_color(theme.colors.text_muted)
                            .child(language),
                    )
                    .child(CopyButton::new((id.clone(), "copy"), block.code.clone())),
            )
            .child(
                div()
                    .id((id.clone(), "scroll"))
                    .overflow_x_scroll()
                    .px(theme.spacing.md)
                    .py(theme.spacing.sm)
                    .whitespace_nowrap()
                    .child(
                        RichText::new(id, text).spans(spans).mono().size(theme.typography.size_sm),
                    ),
            )
            .into_any_element()
    }

    /// Display maths, typeset; the Unicode reading where RaTeX cannot read the TeX.
    fn math(&mut self, block: &MathBlock) -> AnyElement {
        let theme = self.theme;
        let id = self.next_id("math");
        let formula = formula(&block.source, theme.colors.text, self.window.scale_factor());
        let body = match formula {
            Some(formula) => img(formula.image.clone())
                .flex_none()
                .w(px(formula.width))
                .h(px(formula.height))
                .into_any_element(),
            None => RichText::new(id.clone(), block.math.to_unicode())
                .italic()
                .size(theme.typography.size_lg)
                .into_any_element(),
        };
        div()
            .id(id)
            .flex()
            .justify_center()
            .overflow_x_scroll()
            .py(theme.spacing.xs)
            .child(body)
            .into_any_element()
    }
}

/// A task list item's tick, drawn but not clickable: the agent's list is not the person's to
/// edit.
fn task_box(checked: bool, theme: &Theme) -> impl IntoElement + use<> {
    div()
        .mt(px(3.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(14.))
        .rounded(theme.shape.radius_xs)
        .border_1()
        .border_color(if checked { theme.colors.accent } else { theme.colors.border_strong })
        .when(checked, |tick| {
            tick.bg(theme.colors.accent)
                .child(Icon::new(IconName::Check).size(px(10.)).color(theme.colors.on_accent))
        })
}

/// How a highlighted token is set.
fn token_span(class: TokenClass, theme: &Theme) -> Span {
    let colors = &theme.colors;
    let color = |color| Span { color: Some(color), ..Span::default() };
    match class {
        TokenClass::Plain => Span::default(),
        TokenClass::Comment => Span { italic: true, ..color(colors.text_faint) },
        TokenClass::Keyword | TokenClass::Tag => color(colors.danger),
        TokenClass::String => color(colors.success),
        TokenClass::Number | TokenClass::Constant => color(colors.info),
        TokenClass::Function => color(colors.accent_text),
        TokenClass::Type | TokenClass::Attribute => color(colors.warning),
        TokenClass::Operator | TokenClass::Punctuation => color(colors.text_muted),
        TokenClass::Inserted => {
            Span { background: Some(colors.success_wash), ..color(colors.success) }
        }
        TokenClass::Deleted => {
            Span { background: Some(colors.danger_wash), ..color(colors.danger) }
        }
        TokenClass::Heading => Span { bold: true, ..color(colors.info) },
    }
}

/// How a run of Markdown text is set.
fn run_span(run: &Run, theme: &Theme) -> Span {
    let style = &run.style;
    Span {
        color: if style.link.is_some() { Some(theme.colors.accent_text) } else { None },
        background: style.code.then_some(theme.colors.hover),
        bold: style.strong,
        italic: style.emphasis || style.math,
        strike: style.strikethrough,
        underline: style.link.is_some(),
        mono: style.code,
    }
}

/// Display maths drawn to an image.
struct Formula {
    image: Arc<gpui::Image>,
    width: f32,
    height: f32,
}

/// The TeX, the colour and the screen's scale: a change of theme or screen draws anew.
type FormulaKey = (String, [u8; 4], u32);

/// The formula for `tex` in `color`, drawn for a screen of `scale`, or `None` when RaTeX
/// cannot read it. Both are cached, so a failure is not tried again every frame.
fn formula(tex: &str, color: Hsla, scale: f32) -> Option<Rc<Formula>> {
    let rgba = color.to_rgb();
    let channels = [rgba.r, rgba.g, rgba.b, rgba.a].map(|c| (c * 255.).round() as u8);
    let key = (tex.to_owned(), channels, (scale * 100.).round() as u32);
    FORMULAS.with_borrow_mut(|cache| {
        if let Some(formula) = cache.get(&key) {
            return formula.clone();
        }
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        let formula = typeset(tex, rgba, scale).map(Rc::new);
        cache.insert(key, formula.clone());
        formula
    })
}

/// Typeset `tex` in display style with RaTeX.
fn typeset(tex: &str, color: gpui::Rgba, scale: f32) -> Option<Formula> {
    use ratex_layout::{LayoutOptions, layout, to_display_list};
    use ratex_render::{RenderOptions, render_to_png};
    use ratex_types::color::Color;
    use ratex_types::math_style::MathStyle;

    /// A little larger than body text, as displayed maths usually is.
    const FONT_SIZE: f32 = 17.;
    let nodes = ratex_parser::parser::parse(tex).ok()?;
    let options = LayoutOptions::default().with_style(MathStyle::Display).with_color(Color {
        r: color.r,
        g: color.g,
        b: color.b,
        a: color.a,
    });
    let list = to_display_list(&layout(&nodes, &options));
    let scale = scale.clamp(0.5, 8.);
    let png = render_to_png(
        &list,
        &RenderOptions {
            font_size: FONT_SIZE,
            padding: 2.,
            background_color: Color { r: 0., g: 0., b: 0., a: 0. },
            // The fonts are built in.
            font_dir: String::new(),
            device_pixel_ratio: scale,
        },
    )
    .ok()?;
    // The size is in the header, the first chunk at a fixed place.
    if png.len() < 24 || &png[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(png[20..24].try_into().ok()?);
    if width == 0 || height == 0 {
        return None;
    }
    Some(Formula {
        image: Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, png)),
        width: width as f32 / scale,
        height: height as f32 / scale,
    })
}

/// How part of a [`RichText`] is set, over its base style.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Span {
    /// The text colour, if not the base one.
    pub color: Option<Hsla>,
    /// A fill behind the text.
    pub background: Option<Hsla>,
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
    /// Struck through.
    pub strike: bool,
    /// Underlined.
    pub underline: bool,
    /// In the monospaced font.
    pub mono: bool,
}

/// What one [`RichText`] remembers between frames: its selection, as Atelier's
/// `SelectableText` keeps it.
struct Selection {
    focus: FocusHandle,
    text: SharedString,
    anchor: usize,
    head: usize,
    dragging: bool,
    /// The link pressed, opened if the press ends without a drag.
    pressed: Option<SharedString>,
    /// Whether the pointer is over a link.
    over_link: bool,
    layout: Option<TextLayout>,
}

impl Selection {
    fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    fn index_at(&self, position: Point<Pixels>) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let index = layout.index_for_position(position).unwrap_or_else(|index| index);
        Some(floor_boundary(&self.text, index))
    }
}

fn floor_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// The link at `index`, if any.
fn link_at(links: &[(Range<usize>, SharedString)], index: usize) -> Option<SharedString> {
    links.iter().find(|(range, _)| range.contains(&index)).map(|(_, url)| url.clone())
}

/// Styled text the person can select and copy, whose links open on click. Atelier's
/// `SelectableText` draws one style; this draws runs of several, keeping its gestures: drag
/// to select, Shift-click to extend, double-click for a word, ⌘C / Ctrl+C to copy.
#[derive(IntoElement)]
pub struct RichText {
    id: ElementId,
    text: SharedString,
    spans: Vec<(Range<usize>, Span)>,
    links: Vec<(Range<usize>, SharedString)>,
    size: Option<Pixels>,
    weight: Option<FontWeight>,
    mono: bool,
    italic: bool,
}

impl RichText {
    /// `text` in the base style.
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            spans: Vec::new(),
            links: Vec::new(),
            size: None,
            weight: None,
            mono: false,
            italic: false,
        }
    }

    /// A paragraph of Markdown text, with its styles and links.
    fn from_text(id: ElementId, text: &Text, theme: &Theme) -> Self {
        let mut plain = String::new();
        let mut spans = Vec::new();
        let mut links = Vec::new();
        for run in &text.runs {
            let start = plain.len();
            plain.push_str(&run.text);
            let range = start..plain.len();
            if let Some(url) = &run.style.link {
                links.push((range.clone(), SharedString::from(url.clone())));
            }
            spans.push((range, run_span(run, theme)));
        }
        Self { spans, links, ..Self::new(id, plain) }
    }

    /// Styles over parts of the text: byte ranges, in order, not overlapping.
    pub fn spans(mut self, spans: Vec<(Range<usize>, Span)>) -> Self {
        self.spans = spans;
        self
    }

    /// The type size.
    pub fn size(mut self, size: Pixels) -> Self {
        self.size = Some(size);
        self
    }

    /// The weight of the whole text.
    pub fn weight(mut self, weight: FontWeight) -> Self {
        self.weight = Some(weight);
        self
    }

    /// The whole text in the monospaced font.
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    /// The whole text in italics.
    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    /// The runs gpui lays out: the spans, cut where the selection begins and ends.
    fn runs(&self, theme: &Theme, color: Hsla, selection: Option<Range<usize>>) -> Vec<TextRun> {
        let len = self.text.len();
        let mut cuts = vec![0, len];
        for (range, _) in &self.spans {
            cuts.extend([range.start.min(len), range.end.min(len)]);
        }
        if let Some(range) = &selection {
            cuts.extend([range.start, range.end]);
        }
        cuts.sort_unstable();
        cuts.dedup();
        let base = if self.mono { &theme.typography.mono } else { &theme.typography.ui };
        cuts.windows(2)
            .map(|cut| {
                let (start, end) = (cut[0], cut[1]);
                let span = self
                    .spans
                    .iter()
                    .find(|(range, _)| range.start <= start && start < range.end)
                    .map(|(_, span)| span.clone())
                    .unwrap_or_default();
                let mut font = if span.mono { theme.typography.mono.clone() } else { base.clone() };
                if let Some(weight) = self.weight {
                    font.weight = weight;
                }
                if span.bold {
                    font.weight = FontWeight::BOLD;
                }
                if span.italic || self.italic {
                    font.style = FontStyle::Italic;
                }
                let selected = selection.as_ref().is_some_and(|range| range.contains(&start));
                let text_color = span.color.unwrap_or(color);
                TextRun {
                    len: end - start,
                    font,
                    color: text_color,
                    background_color: if selected {
                        Some(theme.colors.selection)
                    } else {
                        span.background
                    },
                    underline: span.underline.then_some(UnderlineStyle {
                        thickness: px(1.),
                        color: Some(text_color.opacity(0.5)),
                        wavy: false,
                    }),
                    strikethrough: span.strike.then_some(StrikethroughStyle {
                        thickness: px(1.),
                        color: Some(text_color),
                    }),
                }
            })
            .collect()
    }
}

impl RenderOnce for RichText {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let color = theme.colors.text;
        let state = window.use_keyed_state(self.id.clone(), cx, |_, cx| Selection {
            focus: cx.focus_handle(),
            text: SharedString::default(),
            anchor: 0,
            head: 0,
            dragging: false,
            pressed: None,
            over_link: false,
            layout: None,
        });
        let text = self.text.clone();
        let selection = state.update(cx, |selection, _| {
            if selection.text != text {
                selection.anchor = floor_boundary(&text, selection.anchor);
                selection.head = floor_boundary(&text, selection.head);
                selection.text = text.clone();
            }
            let range = selection.range();
            (selection.focus.is_focused(window) && !range.is_empty()).then_some(range)
        });
        let styled = StyledText::new(text).with_runs(self.runs(&theme, color, selection));
        let (focus, dragging, over_link) = state.update(cx, |selection, _| {
            selection.layout = Some(styled.layout().clone());
            (selection.focus.clone(), selection.dragging, selection.over_link)
        });
        let links = Rc::new(self.links);

        div()
            .id(self.id.clone())
            .track_focus(&focus)
            .key_context(SELECTABLE_CONTEXT)
            .relative()
            .map(|element| if over_link { element.cursor_pointer() } else { element.cursor_text() })
            .when_some(self.size, |element, size| element.text_size(size))
            .line_height(relative(1.5))
            .on_mouse_down(MouseButton::Left, {
                let (state, links) = (state.clone(), links.clone());
                move |event: &MouseDownEvent, window, cx| {
                    let focus = state.read(cx).focus.clone();
                    window.focus(&focus);
                    state.update(cx, |selection, cx| {
                        let Some(index) = selection.index_at(event.position) else { return };
                        let range = match event.click_count {
                            2 => word_at(&selection.text, index),
                            3.. => 0..selection.text.len(),
                            _ if event.modifiers.shift => selection.anchor..index,
                            _ => index..index,
                        };
                        (selection.anchor, selection.head) = (range.start, range.end);
                        selection.dragging = event.click_count < 2;
                        selection.pressed = (event.click_count == 1 && !event.modifiers.shift)
                            .then(|| link_at(&links, index))
                            .flatten();
                        cx.notify();
                    });
                }
            })
            .on_mouse_move({
                let (state, links) = (state.clone(), links.clone());
                move |event: &MouseMoveEvent, _, cx| {
                    state.update(cx, |selection, cx| {
                        let over = selection
                            .index_at(event.position)
                            .is_some_and(|index| link_at(&links, index).is_some());
                        if over != selection.over_link {
                            selection.over_link = over;
                            cx.notify();
                        }
                    });
                }
            })
            .on_action({
                let state = state.clone();
                move |_: &CopySelection, _, cx| {
                    let selection = state.read(cx);
                    let range = selection.range();
                    if range.is_empty() {
                        cx.propagate();
                        return;
                    }
                    let copied = selection.text[range].to_owned();
                    cx.write_to_clipboard(ClipboardItem::new_string(copied));
                }
            })
            .on_action({
                let state = state.clone();
                move |_: &SelectAll, _, cx| {
                    state.update(cx, |selection, cx| {
                        (selection.anchor, selection.head) = (0, selection.text.len());
                        cx.notify();
                    });
                }
            })
            .child(styled)
            // While a drag is under way, follow the pointer anywhere in the window.
            .when_some(dragging.then_some(state), |element, state| {
                element.child(
                    canvas(
                        |_, _, _| {},
                        move |_, _, window, _| {
                            window.on_mouse_event({
                                let state = state.clone();
                                move |event: &MouseMoveEvent, phase, _, cx| {
                                    if !phase.bubble() || event.pressed_button.is_none() {
                                        return;
                                    }
                                    state.update(cx, |selection, cx| {
                                        if let Some(index) = selection.index_at(event.position)
                                            && index != selection.head
                                        {
                                            selection.head = index;
                                            selection.pressed = None;
                                            cx.notify();
                                        }
                                    });
                                }
                            });
                            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                                if !phase.bubble() {
                                    return;
                                }
                                let link = state.update(cx, |selection, cx| {
                                    selection.dragging = false;
                                    cx.notify();
                                    selection.pressed.take()
                                });
                                if let Some(url) = link {
                                    cx.open_url(&url);
                                }
                            });
                        },
                    )
                    .absolute(),
                )
            })
    }
}

/// The word, or run of spaces or punctuation, around `index`.
fn word_at(text: &str, index: usize) -> Range<usize> {
    let kind = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let Some(at) = text[index..].chars().next().map(kind) else { return index..index };
    let start = text[..index]
        .char_indices()
        .rev()
        .take_while(|(_, c)| kind(*c) == at)
        .last()
        .map_or(index, |(start, _)| start);
    let end = text[index..]
        .char_indices()
        .find(|(_, c)| kind(*c) != at)
        .map_or(text.len(), |(end, _)| index + end);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_around_a_point() {
        let text = "let foo_bar = 1;";
        assert_eq!(&text[word_at(text, 5)], "foo_bar");
        assert_eq!(&text[word_at(text, 0)], "let");
        assert_eq!(word_at(text, text.len()), text.len()..text.len());
    }

    #[test]
    fn the_same_text_is_read_once() {
        let first = document("# Title\n\nbody");
        let again = document("# Title\n\nbody");
        assert!(Rc::ptr_eq(&first, &again));
    }

    #[test]
    fn display_maths_typesets() {
        let typeset = formula(r"\frac{a}{b} + \sqrt{x^2 + 1}", gpui::white(), 2.).unwrap();
        assert!(typeset.width > 10. && typeset.width < 400.);
        assert!(typeset.height > 10. && typeset.height < 200.);
        assert!(formula(r"\frac{a}{", gpui::white(), 2.).is_none());
    }
}
