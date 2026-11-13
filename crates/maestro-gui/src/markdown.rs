//! An agent's answer drawn as rich text: the Markdown, maths and highlighted code that
//! `maestro-markdown` reads, laid out with Atelier's tokens.
//!
//! Reading happens in `maestro-markdown`, which knows nothing of gpui, so a terminal frontend
//! can draw the same model. Only the drawing lives here: text runs, code blocks and tables in
//! the theme, and display maths typeset by RaTeX into an image.
//!
//! Every piece of text is an Atelier `SelectableText`, so a selection runs from one block into
//! the next, and on into the next message when the transcript is a `SelectionScope`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

use atelier::{
    ActiveTheme, BLOCK_SEPARATOR, CopyButton, Icon, IconName, LINE_SEPARATOR, SelectableText,
    SelectionScope, TextSize, Theme,
};
use gpui::{
    AnyElement, App, ElementId, FontStyle, FontWeight, HighlightStyle, Hsla, InteractiveElement,
    IntoElement, ParentElement, RenderOnce, SharedString, StatefulInteractiveElement,
    StrikethroughStyle, Styled, UnderlineStyle, Window, div, img, prelude::FluentBuilder, px,
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

/// A reply drawn as rich text. A selection runs across its blocks and is copied as text.
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
        let mut painter = Painter {
            id: self.id.clone(),
            counter: &mut counter,
            theme: &theme,
            window,
            separator: None,
        };
        // A scope of its own when drawn alone; inside the transcript's it joins that one.
        SelectionScope::new(self.id)
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
    /// What the next piece of text puts before itself when copied with the one before, if not
    /// a blank line.
    separator: Option<&'static str>,
}

impl Painter<'_> {
    fn next_id(&mut self, kind: &str) -> ElementId {
        *self.counter += 1;
        ElementId::NamedChild(Box::new(self.id.clone()), format!("{kind}-{}", self.counter).into())
    }

    /// A run of Markdown text, selectable with its styles and links.
    fn text(&mut self, kind: &str, text: &Text, size: TextSize) -> SelectableText {
        let separator = self.separator.take().unwrap_or(BLOCK_SEPARATOR);
        let mut plain = String::new();
        let mut styles = Vec::new();
        let mut links = Vec::new();
        for run in &text.runs {
            let start = plain.len();
            plain.push_str(&run.text);
            let range = start..plain.len();
            if let Some(url) = &run.style.link {
                links.push((range.clone(), SharedString::from(url.clone())));
            }
            styles.push((range, run_style(run, self.theme)));
        }
        let mut selectable = SelectableText::new(self.next_id(kind), plain)
            .separator(separator)
            .size(size)
            .styles(styles);
        for (range, url) in links {
            selectable = selectable.link(range, url);
        }
        selectable
    }

    fn blocks(&mut self, blocks: &[Block]) -> Vec<AnyElement> {
        blocks.iter().map(|block| self.block(block)).collect()
    }

    fn block(&mut self, block: &Block) -> AnyElement {
        let theme = self.theme;
        match block {
            Block::Paragraph(text) => {
                div().min_w_0().child(self.text("p", text, TextSize::Medium)).into_any_element()
            }
            Block::Heading { level, text } => {
                let size = match level {
                    1 => TextSize::XLarge,
                    2 => TextSize::Large,
                    _ => TextSize::Medium,
                };
                div()
                    .min_w_0()
                    .when(*level <= 2, |heading| heading.pt(theme.spacing.xs))
                    .font_weight(theme.typography.heading_weight)
                    .child(self.text("h", text, size))
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
                // Items copy one to a line, like the list they came from.
                if index > 0 {
                    self.separator = Some(LINE_SEPARATOR);
                }
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
                // A row copies to a line, its cells apart by tabs, as a spreadsheet pastes it.
                if index > 0 || column > 0 {
                    self.separator = Some(if column == 0 { LINE_SEPARATOR } else { "\t" });
                }
                let rich = self.text("cell", &text, TextSize::Small);
                cells.push(
                    div()
                        .flex()
                        .min_w_0()
                        .px(theme.spacing.sm)
                        .py(theme.spacing.xs)
                        .when(!last, |cell| cell.border_b_1().border_color(theme.colors.border))
                        .when(header, |cell| {
                            cell.bg(theme.colors.sunken).font_weight(theme.typography.label_weight)
                        })
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
        let mut styles = Vec::new();
        for (index, line) in block.lines.iter().enumerate() {
            if index > 0 {
                text.push('\n');
            }
            for token in line {
                let start = text.len();
                text.push_str(&token.text);
                styles.push((start..text.len(), token_style(token.class, theme)));
            }
        }
        let separator = self.separator.take().unwrap_or(BLOCK_SEPARATOR);
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
                        SelectableText::new(id, text)
                            .separator(separator)
                            .mono()
                            .size(TextSize::Small)
                            .styles(styles),
                    ),
            )
            .into_any_element()
    }

    /// Display maths, typeset; the Unicode reading where RaTeX cannot read the TeX.
    fn math(&mut self, block: &MathBlock) -> AnyElement {
        let theme = self.theme;
        let id = self.next_id("math");
        let formula = formula(&block.source, theme.colors.text, self.window.scale_factor());
        let separator = self.separator.take().unwrap_or(BLOCK_SEPARATOR);
        let body = match formula {
            Some(formula) => img(formula.image.clone())
                .flex_none()
                .w(px(formula.width))
                .h(px(formula.height))
                .into_any_element(),
            None => {
                let text = block.math.to_unicode();
                let italic = HighlightStyle {
                    font_style: Some(FontStyle::Italic),
                    ..HighlightStyle::default()
                };
                SelectableText::new((id.clone(), "text"), text.clone())
                    .separator(separator)
                    .size(TextSize::Large)
                    .styles([(0..text.len(), italic)])
                    .into_any_element()
            }
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
fn token_style(class: TokenClass, theme: &Theme) -> HighlightStyle {
    let colors = &theme.colors;
    let color = |color| HighlightStyle { color: Some(color), ..HighlightStyle::default() };
    match class {
        TokenClass::Plain => HighlightStyle::default(),
        TokenClass::Comment => {
            HighlightStyle { font_style: Some(FontStyle::Italic), ..color(colors.text_faint) }
        }
        TokenClass::Keyword | TokenClass::Tag => color(colors.danger),
        TokenClass::String => color(colors.success),
        TokenClass::Number | TokenClass::Constant => color(colors.info),
        TokenClass::Function => color(colors.accent_text),
        TokenClass::Type | TokenClass::Attribute => color(colors.warning),
        TokenClass::Operator | TokenClass::Punctuation => color(colors.text_muted),
        TokenClass::Inserted => {
            HighlightStyle { background_color: Some(colors.success_wash), ..color(colors.success) }
        }
        TokenClass::Deleted => {
            HighlightStyle { background_color: Some(colors.danger_wash), ..color(colors.danger) }
        }
        TokenClass::Heading => {
            HighlightStyle { font_weight: Some(FontWeight::BOLD), ..color(colors.info) }
        }
    }
}

/// How a run of Markdown text is set: as Atelier's own Markdown sets it, with inline code on
/// a wash (a highlight cannot change the font) and inline maths in italics.
fn run_style(run: &Run, theme: &Theme) -> HighlightStyle {
    let style = &run.style;
    let link = style.link.is_some().then_some(theme.colors.accent_text);
    HighlightStyle {
        color: link,
        background_color: style.code.then_some(theme.colors.hover),
        font_weight: style.strong.then_some(FontWeight::BOLD),
        font_style: (style.emphasis || style.math).then_some(FontStyle::Italic),
        strikethrough: style
            .strikethrough
            .then_some(StrikethroughStyle { thickness: px(1.), color: None }),
        underline: link.map(|color| UnderlineStyle {
            thickness: px(1.),
            color: Some(color.opacity(0.45)),
            wavy: false,
        }),
        ..HighlightStyle::default()
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

#[cfg(test)]
mod tests {
    use super::*;

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
