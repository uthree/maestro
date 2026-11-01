//! TeX math, read into a small tree a frontend lays out, or flattens to Unicode where it can
//! only draw a line of text.
//!
//! The reader covers what agents write in answers: letters, numbers and operators, `^` and `_`,
//! `\frac`, `\sqrt`, `\left`/`\right`, the matrix environments, accents, font commands and
//! the named symbols. It never fails: a command it does not know comes back as
//! [`Math::Unknown`], drawn as written.

use std::iter::Peekable;
use std::str::Chars;

/// One piece of a formula.
#[derive(Clone, Debug, PartialEq)]
pub enum Math {
    /// Pieces set side by side.
    Row(Vec<Math>),
    /// A variable, set in italics: `x`, `α`.
    Ident(String),
    /// Digits, with any decimal point.
    Number(String),
    /// An upright symbol that takes no extra space around it: `(`, `,`, `∞`, `Γ`, `ℝ`.
    Symbol(String),
    /// A binary operator or relation, with a little space either side: `+`, `=`, `≤`, `→`.
    Operator(String),
    /// A named function set upright: `sin`, `log`, `lim`.
    Function {
        /// The name as shown.
        name: String,
        /// Whether, in a display, its scripts go under and over it, as with `lim`.
        limits: bool,
    },
    /// A large operator: `∑`, `∫`, `⋃`.
    LargeOp {
        /// The symbol.
        symbol: String,
        /// Whether, in a display, its scripts go under and over it, as with `∑` but not `∫`.
        limits: bool,
    },
    /// Upright words inside a formula, from `\text{…}`.
    Text(String),
    /// A fraction, or with no line a binomial's stack.
    Frac {
        /// Over the line.
        numerator: Box<Math>,
        /// Under the line.
        denominator: Box<Math>,
        /// Whether the line is drawn.
        line: bool,
    },
    /// A root.
    Sqrt {
        /// The degree, as in `\sqrt[3]{x}`.
        index: Option<Box<Math>>,
        /// What is under the sign.
        radicand: Box<Math>,
    },
    /// A base with a subscript, a superscript or both.
    Scripts {
        /// What the scripts belong to.
        base: Box<Math>,
        /// Below and to the right.
        sub: Option<Box<Math>>,
        /// Above and to the right.
        sup: Option<Box<Math>>,
    },
    /// A body between delimiters that grow with it, from `\left`/`\right`. Either may be empty.
    Fenced {
        /// The opening delimiter.
        open: String,
        /// The closing delimiter.
        close: String,
        /// Between them.
        body: Box<Math>,
    },
    /// Rows of cells: a matrix, `cases`, or aligned equations.
    Table {
        /// The cells, row by row.
        rows: Vec<Vec<Math>>,
        /// The delimiter before it, or empty.
        open: String,
        /// The delimiter after it, or empty.
        close: String,
        /// How each cell sits in its column.
        align: CellAlign,
    },
    /// A mark over (or a line under) its body: `\hat`, `\vec`, `\overline`.
    Accent {
        /// Which mark.
        accent: Accent,
        /// What it marks.
        body: Box<Math>,
    },
    /// Horizontal space, in ems.
    Space(f32),
    /// A command this reader does not know, kept as written.
    Unknown(String),
}

/// How the cells of a [`Math::Table`] sit in their columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellAlign {
    /// Centred, as in a matrix.
    Center,
    /// Left-aligned, as in `cases`.
    Left,
    /// Alternately right and left, as in `aligned`, so relations line up.
    Aligned,
}

/// The marks of a [`Math::Accent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accent {
    /// `\hat`, `\widehat`: x̂
    Hat,
    /// `\bar`: x̄
    Bar,
    /// `\overline`: a line over the whole body.
    Overline,
    /// `\underline`: a line under the whole body.
    Underline,
    /// `\vec`, `\overrightarrow`: an arrow.
    Vec,
    /// `\dot`: ẋ
    Dot,
    /// `\ddot`: ẍ
    Ddot,
    /// `\tilde`, `\widetilde`: x̃
    Tilde,
}

impl Accent {
    /// The mark as a character drawn on its own, over the body.
    pub fn mark(self) -> &'static str {
        match self {
            Accent::Hat => "^",
            Accent::Bar | Accent::Overline | Accent::Underline => "‾",
            Accent::Vec => "→",
            Accent::Dot => "˙",
            Accent::Ddot => "¨",
            Accent::Tilde => "~",
        }
    }

    /// The combining character that puts the mark on the letter before it.
    fn combining(self) -> char {
        match self {
            Accent::Hat => '\u{302}',
            Accent::Bar | Accent::Overline => '\u{305}',
            Accent::Underline => '\u{332}',
            Accent::Vec => '\u{20D7}',
            Accent::Dot => '\u{307}',
            Accent::Ddot => '\u{308}',
            Accent::Tilde => '\u{303}',
        }
    }
}

/// Read TeX math. Never fails; see the module notes.
pub fn parse(source: &str) -> Math {
    let mut reader = Reader { chars: source.chars().peekable() };
    let rows = reader.table(Stop::End);
    // `&` and `\\` outside an environment: read the whole formula as aligned lines.
    if rows.len() > 1 || rows.first().is_some_and(|row| row.len() > 1) {
        return Math::Table {
            rows,
            open: String::new(),
            close: String::new(),
            align: CellAlign::Aligned,
        };
    }
    rows.into_iter().flatten().next().map(simplify).unwrap_or(Math::Row(Vec::new()))
}

/// A one-item row is its item.
fn simplify(math: Math) -> Math {
    match math {
        Math::Row(mut items) if items.len() == 1 => items.pop().unwrap_or(Math::Row(Vec::new())),
        other => other,
    }
}

/// What ends the row being read.
#[derive(Clone, Copy, PartialEq)]
enum Stop {
    /// The end of the source.
    End,
    /// A closing brace.
    Brace,
    /// `\right`.
    Right,
    /// A closing `]`, for the degree of a root.
    Bracket,
}

/// What a row ended on.
#[derive(PartialEq)]
enum Ended {
    /// Its stop (or the end of the source).
    Stop,
    /// `&`: the next cell.
    Cell,
    /// `\\`: the next row.
    Row,
    /// `\end{…}`.
    Environment,
}

/// One unit of the source.
enum Token {
    /// A command: `\frac`, or `\,` for one that is not a word.
    Command(String),
    /// Any other character.
    Char(char),
}

struct Reader<'a> {
    chars: Peekable<Chars<'a>>,
}

impl Reader<'_> {
    fn skip_space(&mut self) {
        while self.chars.peek().is_some_and(|c| c.is_whitespace()) {
            self.chars.next();
        }
    }

    fn peek_char(&mut self) -> Option<char> {
        self.skip_space();
        self.chars.peek().copied()
    }

    fn token(&mut self) -> Option<Token> {
        self.skip_space();
        let c = self.chars.next()?;
        if c != '\\' {
            return Some(Token::Char(c));
        }
        let mut name = String::new();
        while let Some(&c) = self.chars.peek() {
            if !c.is_ascii_alphabetic() {
                break;
            }
            name.push(c);
            self.chars.next();
        }
        if name.is_empty() {
            match self.chars.next() {
                Some(c) => name.push(c),
                None => name.push('\\'),
            }
        }
        Some(Token::Command(name))
    }

    /// The raw text of a `{…}` group, braces balanced, spaces kept; or the next character.
    fn raw_group(&mut self) -> String {
        if self.peek_char() != Some('{') {
            return self.chars.next().map(String::from).unwrap_or_default();
        }
        self.chars.next();
        let mut depth = 0;
        let mut text = String::new();
        for c in self.chars.by_ref() {
            match c {
                '{' => depth += 1,
                '}' if depth == 0 => break,
                '}' => depth -= 1,
                _ => {}
            }
            text.push(c);
        }
        text
    }

    /// Cells and rows up to `stop`, split at `&` and `\\`.
    fn table(&mut self, stop: Stop) -> Vec<Vec<Math>> {
        let mut rows = Vec::new();
        let mut cells = Vec::new();
        loop {
            let (cell, ended) = self.row(stop);
            cells.push(cell);
            match ended {
                Ended::Cell => {}
                Ended::Row => rows.push(std::mem::take(&mut cells)),
                Ended::Stop | Ended::Environment => break,
            }
        }
        // A trailing `\\` leaves an empty last row.
        if !(cells.len() == 1 && cells[0] == Math::Row(Vec::new())) || rows.is_empty() {
            rows.push(cells);
        }
        rows
    }

    /// One row of pieces, up to `stop`, a cell or row break, or `\end`.
    fn row(&mut self, stop: Stop) -> (Math, Ended) {
        let mut items = Vec::new();
        loop {
            match self.peek_char() {
                None => return (Math::Row(items), Ended::Stop),
                Some('}') => {
                    self.chars.next();
                    if stop == Stop::Brace {
                        return (Math::Row(items), Ended::Stop);
                    }
                    // A stray brace closes nothing.
                }
                Some(']') if stop == Stop::Bracket => {
                    self.chars.next();
                    return (Math::Row(items), Ended::Stop);
                }
                Some('&') => {
                    self.chars.next();
                    return (Math::Row(items), Ended::Cell);
                }
                Some('\\') => {
                    let mut ahead = self.chars.clone();
                    ahead.next();
                    let mut name = String::new();
                    while let Some(&c) = ahead.peek() {
                        if !c.is_ascii_alphabetic() {
                            break;
                        }
                        name.push(c);
                        ahead.next();
                    }
                    if name.is_empty() && ahead.peek() == Some(&'\\') {
                        self.token();
                        return (Math::Row(items), Ended::Row);
                    }
                    match name.as_str() {
                        "right" if stop == Stop::Right => {
                            self.token();
                            return (Math::Row(items), Ended::Stop);
                        }
                        "end" => {
                            self.token();
                            self.raw_group();
                            return (Math::Row(items), Ended::Environment);
                        }
                        "cr" | "newline" => {
                            self.token();
                            return (Math::Row(items), Ended::Row);
                        }
                        _ => {}
                    }
                    if let Some(item) = self.scripted() {
                        items.push(item);
                    }
                }
                Some(_) => {
                    if let Some(item) = self.scripted() {
                        items.push(item);
                    }
                }
            }
        }
    }

    /// An atom with the scripts and primes that follow it.
    fn scripted(&mut self) -> Option<Math> {
        let mut base = self.atom()?;
        let (mut sub, mut sup): (Option<Math>, Option<Math>) = (None, None);
        loop {
            match self.peek_char() {
                Some('^') => {
                    self.chars.next();
                    let script = self.argument();
                    if script == Math::Row(Vec::new()) {
                        continue;
                    }
                    sup = Some(match sup.take() {
                        Some(Math::Row(mut primes)) => {
                            primes.push(script);
                            Math::Row(primes)
                        }
                        Some(earlier) => Math::Row(vec![earlier, script]),
                        None => script,
                    });
                }
                Some('_') => {
                    self.chars.next();
                    let script = self.argument();
                    if script != Math::Row(Vec::new()) {
                        sub = Some(script);
                    }
                }
                Some('\'') => {
                    self.chars.next();
                    let prime = Math::Symbol("′".into());
                    sup = Some(match sup.take() {
                        Some(Math::Row(mut primes)) => {
                            primes.push(prime);
                            Math::Row(primes)
                        }
                        Some(earlier) => Math::Row(vec![earlier, prime]),
                        None => Math::Row(vec![prime]),
                    });
                }
                _ => break,
            }
            if matches!(base, Math::Scripts { .. }) {
                // `{x^a}^b`: scripts on scripts stay nested.
                base = Math::Row(vec![base]);
            }
        }
        if sub.is_none() && sup.is_none() {
            return Some(base);
        }
        Some(Math::Scripts {
            base: Box::new(base),
            sub: sub.map(|sub| Box::new(simplify(sub))),
            sup: sup.map(|sup| Box::new(simplify(sup))),
        })
    }

    /// The argument of a command or a script: a group or a single atom, without scripts.
    fn argument(&mut self) -> Math {
        // As in TeX, a bare number argument is one digit: `\frac12` is ½.
        if let Some(digit) = self.peek_char().filter(char::is_ascii_digit) {
            self.chars.next();
            return Math::Number(digit.into());
        }
        self.atom().map(simplify).unwrap_or(Math::Row(Vec::new()))
    }

    /// A group in braces, or nothing if the next thing is not one.
    fn optional_bracket(&mut self) -> Option<Math> {
        if self.peek_char() != Some('[') {
            return None;
        }
        self.chars.next();
        Some(simplify(self.row(Stop::Bracket).0))
    }

    /// One piece, without its scripts. `None` only at the end of the source.
    fn atom(&mut self) -> Option<Math> {
        let token = self.token()?;
        Some(match token {
            Token::Char('{') => simplify(self.row(Stop::Brace).0),
            Token::Char(c) if c.is_ascii_digit() || c == '.' => {
                let mut number = String::from(c);
                while let Some(&next) = self.chars.peek() {
                    if !(next.is_ascii_digit() || next == '.') {
                        break;
                    }
                    number.push(next);
                    self.chars.next();
                }
                if number == "." { Math::Symbol(number) } else { Math::Number(number) }
            }
            Token::Char(c) if c.is_alphabetic() => Math::Ident(c.into()),
            Token::Char(c) => char_atom(c),
            Token::Command(name) => self.command(&name),
        })
    }

    fn command(&mut self, name: &str) -> Math {
        match name {
            "frac" | "dfrac" | "tfrac" | "cfrac" => {
                let numerator = Box::new(self.argument());
                let denominator = Box::new(self.argument());
                Math::Frac { numerator, denominator, line: true }
            }
            "binom" | "dbinom" | "tbinom" => {
                let numerator = Box::new(self.argument());
                let denominator = Box::new(self.argument());
                Math::Fenced {
                    open: "(".into(),
                    close: ")".into(),
                    body: Box::new(Math::Frac { numerator, denominator, line: false }),
                }
            }
            "sqrt" => {
                let index = self.optional_bracket().map(Box::new);
                Math::Sqrt { index, radicand: Box::new(self.argument()) }
            }
            "left" => {
                let open = self.delimiter();
                let body = simplify(self.row(Stop::Right).0);
                let close = self.delimiter();
                Math::Fenced { open, close, body: Box::new(body) }
            }
            // A stray `\right` is just its delimiter.
            "right" => Math::Symbol(self.delimiter()),
            "begin" => {
                let environment = self.raw_group();
                self.environment(environment.trim())
            }
            "text" | "textrm" | "textup" | "textnormal" | "mbox" | "hbox" | "textit" | "textbf"
            | "textsf" | "texttt" | "emph" => Math::Text(self.raw_group()),
            "mathrm" | "operatorname" | "mathup" | "rm" => {
                let name = self.raw_group();
                let name: String = name.chars().filter(|c| !c.is_whitespace()).collect();
                if name.chars().all(char::is_alphabetic) {
                    Math::Function { name, limits: false }
                } else {
                    Math::Text(name)
                }
            }
            "mathbb" | "Bbb" | "mathbf" | "boldsymbol" | "bm" | "mathcal" | "mathscr"
            | "mathfrak" | "mathsf" | "mathtt" | "mathit" => {
                let font = Font::of(name);
                restyle(self.argument(), font)
            }
            "hat" | "widehat" => self.accent(Accent::Hat),
            "bar" => self.accent(Accent::Bar),
            "overline" => self.accent(Accent::Overline),
            "underline" => self.accent(Accent::Underline),
            "vec" | "overrightarrow" => self.accent(Accent::Vec),
            "dot" => self.accent(Accent::Dot),
            "ddot" => self.accent(Accent::Ddot),
            "tilde" | "widetilde" => self.accent(Accent::Tilde),
            "overbrace" | "underbrace" | "boxed" | "phantom" | "mathstrut" => self.argument(),
            "color" => {
                self.raw_group();
                Math::Row(Vec::new())
            }
            "textcolor" => {
                self.raw_group();
                self.argument()
            }
            "label" => {
                self.raw_group();
                Math::Row(Vec::new())
            }
            "tag" => Math::Text(format!("({})", self.raw_group())),
            "not" => match self.argument() {
                Math::Operator(op) | Math::Symbol(op) => Math::Operator(format!("{op}\u{338}")),
                other => other,
            },
            // Sizing and style switches change nothing this reader draws.
            "displaystyle" | "textstyle" | "scriptstyle" | "scriptscriptstyle" | "limits"
            | "nolimits" | "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr" | "Bigl" | "Bigr"
            | "biggl" | "biggr" | "Biggl" | "Biggr" | "bigm" | "Bigm" | "middle" | "nonumber"
            | "notag" | "relax" => Math::Row(Vec::new()),
            "," | "thinspace" => Math::Space(0.17),
            ":" | ">" | "medspace" => Math::Space(0.22),
            ";" | "thickspace" => Math::Space(0.28),
            " " | "space" => Math::Space(0.33),
            "quad" => Math::Space(1.),
            "qquad" => Math::Space(2.),
            "!" | "negthinspace" => Math::Space(0.),
            _ => symbol(name).unwrap_or_else(|| Math::Unknown(format!("\\{name}"))),
        }
    }

    fn accent(&mut self, accent: Accent) -> Math {
        Math::Accent { accent, body: Box::new(self.argument()) }
    }

    /// The delimiter after `\left` or `\right`; `.` is none.
    fn delimiter(&mut self) -> String {
        match self.token() {
            Some(Token::Char('.')) | None => String::new(),
            Some(Token::Char(c)) => char_atom(c).text().unwrap_or_default(),
            Some(Token::Command(name)) => match name.as_str() {
                "{" | "lbrace" => "{".into(),
                "}" | "rbrace" => "}".into(),
                "|" | "Vert" | "lVert" | "rVert" => "‖".into(),
                "vert" | "lvert" | "rvert" => "|".into(),
                other => symbol(other).and_then(|math| math.text()).unwrap_or_default(),
            },
        }
    }

    fn environment(&mut self, name: &str) -> Math {
        let (open, close, align) = match name {
            "pmatrix" | "pmatrix*" => ("(", ")", CellAlign::Center),
            "bmatrix" | "bmatrix*" => ("[", "]", CellAlign::Center),
            "Bmatrix" => ("{", "}", CellAlign::Center),
            "vmatrix" => ("|", "|", CellAlign::Center),
            "Vmatrix" => ("‖", "‖", CellAlign::Center),
            "cases" | "dcases" => ("{", "", CellAlign::Left),
            "rcases" => ("", "}", CellAlign::Left),
            "matrix" | "smallmatrix" | "array" | "subarray" | "gathered" | "gather" | "gather*" => {
                ("", "", CellAlign::Center)
            }
            _ => ("", "", CellAlign::Aligned),
        };
        if name == "array" || name == "subarray" {
            // The column specification, `{cc|l}`.
            self.raw_group();
        }
        let mut rows = self.table(Stop::End);
        for row in &mut rows {
            for cell in row.iter_mut() {
                *cell = simplify(std::mem::replace(cell, Math::Row(Vec::new())));
            }
        }
        Math::Table { rows, open: open.into(), close: close.into(), align }
    }
}

/// A character outside any command.
fn char_atom(c: char) -> Math {
    match c {
        '+' | '=' | '<' | '>' => Math::Operator(c.into()),
        '-' => Math::Operator("−".into()),
        '*' => Math::Operator("∗".into()),
        '~' => Math::Space(0.33),
        _ => Math::Symbol(c.into()),
    }
}

/// A command that stands for one symbol, a function name or a large operator.
fn symbol(name: &str) -> Option<Math> {
    let greek = |s: &str| Some(Math::Ident(s.into()));
    let upright = |s: &str| Some(Math::Symbol(s.into()));
    let op = |s: &str| Some(Math::Operator(s.into()));
    let large = |s: &str, limits| Some(Math::LargeOp { symbol: s.into(), limits });
    match name {
        "alpha" => greek("α"),
        "beta" => greek("β"),
        "gamma" => greek("γ"),
        "delta" => greek("δ"),
        "epsilon" => greek("ϵ"),
        "varepsilon" => greek("ε"),
        "zeta" => greek("ζ"),
        "eta" => greek("η"),
        "theta" => greek("θ"),
        "vartheta" => greek("ϑ"),
        "iota" => greek("ι"),
        "kappa" => greek("κ"),
        "lambda" => greek("λ"),
        "mu" => greek("μ"),
        "nu" => greek("ν"),
        "xi" => greek("ξ"),
        "omicron" => greek("ο"),
        "pi" => greek("π"),
        "varpi" => greek("ϖ"),
        "rho" => greek("ρ"),
        "varrho" => greek("ϱ"),
        "sigma" => greek("σ"),
        "varsigma" => greek("ς"),
        "tau" => greek("τ"),
        "upsilon" => greek("υ"),
        "phi" => greek("ϕ"),
        "varphi" => greek("φ"),
        "chi" => greek("χ"),
        "psi" => greek("ψ"),
        "omega" => greek("ω"),
        "ell" => greek("ℓ"),
        "Gamma" => upright("Γ"),
        "Delta" => upright("Δ"),
        "Theta" => upright("Θ"),
        "Lambda" => upright("Λ"),
        "Xi" => upright("Ξ"),
        "Pi" => upright("Π"),
        "Sigma" => upright("Σ"),
        "Upsilon" => upright("Υ"),
        "Phi" => upright("Φ"),
        "Psi" => upright("Ψ"),
        "Omega" => upright("Ω"),
        "infty" => upright("∞"),
        "partial" => upright("∂"),
        "nabla" => upright("∇"),
        "hbar" => upright("ℏ"),
        "emptyset" | "varnothing" => upright("∅"),
        "aleph" => upright("ℵ"),
        "Re" => upright("ℜ"),
        "Im" => upright("ℑ"),
        "angle" => upright("∠"),
        "triangle" => upright("△"),
        "prime" => upright("′"),
        "degree" => upright("°"),
        "ldots" | "dots" | "dotsc" | "dotsb" => upright("…"),
        "cdots" => upright("⋯"),
        "vdots" => upright("⋮"),
        "ddots" => upright("⋱"),
        "langle" => upright("⟨"),
        "rangle" => upright("⟩"),
        "lfloor" => upright("⌊"),
        "rfloor" => upright("⌋"),
        "lceil" => upright("⌈"),
        "rceil" => upright("⌉"),
        "{" | "lbrace" => upright("{"),
        "}" | "rbrace" => upright("}"),
        "|" | "Vert" | "lVert" | "rVert" => upright("‖"),
        "vert" | "lvert" | "rvert" | "mid" => upright("|"),
        "%" | "#" | "$" | "&" | "_" => upright(name),
        "backslash" | "setminus" => op("∖"),
        "colon" => upright(":"),
        "forall" => upright("∀"),
        "exists" => upright("∃"),
        "nexists" => upright("∄"),
        "neg" | "lnot" => upright("¬"),
        "top" => upright("⊤"),
        "bot" | "perp" => upright("⊥"),
        "checkmark" => upright("✓"),
        "pm" => op("±"),
        "mp" => op("∓"),
        "times" => op("×"),
        "div" => op("÷"),
        "cdot" => op("⋅"),
        "ast" => op("∗"),
        "star" => op("⋆"),
        "circ" => op("∘"),
        "bullet" => op("∙"),
        "oplus" => op("⊕"),
        "ominus" => op("⊖"),
        "otimes" => op("⊗"),
        "odot" => op("⊙"),
        "cup" => op("∪"),
        "cap" => op("∩"),
        "wedge" | "land" => op("∧"),
        "vee" | "lor" => op("∨"),
        "leq" | "le" => op("≤"),
        "geq" | "ge" => op("≥"),
        "leqslant" => op("⩽"),
        "geqslant" => op("⩾"),
        "neq" | "ne" => op("≠"),
        "ll" => op("≪"),
        "gg" => op("≫"),
        "approx" => op("≈"),
        "sim" => op("∼"),
        "simeq" => op("≃"),
        "cong" => op("≅"),
        "equiv" => op("≡"),
        "propto" => op("∝"),
        "doteq" => op("≐"),
        "in" => op("∈"),
        "notin" => op("∉"),
        "ni" => op("∋"),
        "subset" => op("⊂"),
        "supset" => op("⊃"),
        "subseteq" => op("⊆"),
        "supseteq" => op("⊇"),
        "subsetneq" => op("⊊"),
        "parallel" => op("∥"),
        "to" | "rightarrow" => op("→"),
        "gets" | "leftarrow" => op("←"),
        "leftrightarrow" => op("↔"),
        "Rightarrow" | "implies" => op("⇒"),
        "Leftarrow" | "impliedby" => op("⇐"),
        "Leftrightarrow" | "iff" => op("⇔"),
        "longrightarrow" => op("⟶"),
        "longleftarrow" => op("⟵"),
        "Longrightarrow" => op("⟹"),
        "Longleftrightarrow" => op("⟺"),
        "mapsto" => op("↦"),
        "uparrow" => op("↑"),
        "downarrow" => op("↓"),
        "hookrightarrow" => op("↪"),
        "vdash" => op("⊢"),
        "models" => op("⊨"),
        "coloneqq" => op("≔"),
        "bmod" | "mod" => Some(Math::Function { name: "mod".into(), limits: false }),
        "sum" => large("∑", true),
        "prod" => large("∏", true),
        "coprod" => large("∐", true),
        "bigcup" => large("⋃", true),
        "bigcap" => large("⋂", true),
        "bigoplus" => large("⨁", true),
        "bigotimes" => large("⨂", true),
        "bigvee" => large("⋁", true),
        "bigwedge" => large("⋀", true),
        "int" => large("∫", false),
        "iint" => large("∬", false),
        "iiint" => large("∭", false),
        "oint" => large("∮", false),
        "lim" | "limsup" | "liminf" | "max" | "min" | "sup" | "inf" | "det" | "gcd" | "Pr"
        | "argmax" | "argmin" => Some(Math::Function {
            name: match name {
                "limsup" => "lim sup".into(),
                "liminf" => "lim inf".into(),
                "argmax" => "arg max".into(),
                "argmin" => "arg min".into(),
                _ => name.into(),
            },
            limits: true,
        }),
        "sin" | "cos" | "tan" | "cot" | "sec" | "csc" | "arcsin" | "arccos" | "arctan" | "sinh"
        | "cosh" | "tanh" | "coth" | "log" | "ln" | "lg" | "exp" | "dim" | "ker" | "deg"
        | "arg" | "hom" | "sgn" | "tr" | "rank" => {
            Some(Math::Function { name: name.into(), limits: false })
        }
        _ => None,
    }
}

/// The type styles of `\mathbb` and its kin.
#[derive(Clone, Copy)]
enum Font {
    DoubleStruck,
    Bold,
    Script,
    Fraktur,
    SansSerif,
    Monospace,
    Italic,
}

impl Font {
    fn of(command: &str) -> Font {
        match command {
            "mathbb" | "Bbb" => Font::DoubleStruck,
            "mathbf" | "boldsymbol" | "bm" => Font::Bold,
            "mathcal" | "mathscr" => Font::Script,
            "mathfrak" => Font::Fraktur,
            "mathsf" => Font::SansSerif,
            "mathtt" => Font::Monospace,
            _ => Font::Italic,
        }
    }

    /// The letter or digit in this style, from Unicode's mathematical alphanumerics.
    fn apply(self, c: char) -> char {
        let exception = match (self, c) {
            (Font::DoubleStruck, 'C') => Some('ℂ'),
            (Font::DoubleStruck, 'H') => Some('ℍ'),
            (Font::DoubleStruck, 'N') => Some('ℕ'),
            (Font::DoubleStruck, 'P') => Some('ℙ'),
            (Font::DoubleStruck, 'Q') => Some('ℚ'),
            (Font::DoubleStruck, 'R') => Some('ℝ'),
            (Font::DoubleStruck, 'Z') => Some('ℤ'),
            (Font::Script, 'B') => Some('ℬ'),
            (Font::Script, 'E') => Some('ℰ'),
            (Font::Script, 'F') => Some('ℱ'),
            (Font::Script, 'H') => Some('ℋ'),
            (Font::Script, 'I') => Some('ℐ'),
            (Font::Script, 'L') => Some('ℒ'),
            (Font::Script, 'M') => Some('ℳ'),
            (Font::Script, 'R') => Some('ℛ'),
            (Font::Script, 'e') => Some('ℯ'),
            (Font::Script, 'g') => Some('ℊ'),
            (Font::Script, 'o') => Some('ℴ'),
            (Font::Fraktur, 'C') => Some('ℭ'),
            (Font::Fraktur, 'H') => Some('ℌ'),
            (Font::Fraktur, 'I') => Some('ℑ'),
            (Font::Fraktur, 'R') => Some('ℜ'),
            (Font::Fraktur, 'Z') => Some('ℨ'),
            (Font::Italic, 'h') => Some('ℎ'),
            _ => None,
        };
        if let Some(c) = exception {
            return c;
        }
        let (upper, lower, digit) = match self {
            Font::Bold => (0x1D400, 0x1D41A, Some(0x1D7CE)),
            Font::Italic => (0x1D434, 0x1D44E, None),
            Font::Script => (0x1D49C, 0x1D4B6, None),
            Font::Fraktur => (0x1D504, 0x1D51E, None),
            Font::DoubleStruck => (0x1D538, 0x1D552, Some(0x1D7D8)),
            Font::SansSerif => (0x1D5A0, 0x1D5BA, Some(0x1D7E2)),
            Font::Monospace => (0x1D670, 0x1D68A, Some(0x1D7F6)),
        };
        let code = match c {
            'A'..='Z' => upper + (c as u32 - 'A' as u32),
            'a'..='z' => lower + (c as u32 - 'a' as u32),
            '0'..='9' => match digit {
                Some(digit) => digit + (c as u32 - '0' as u32),
                None => return c,
            },
            _ => return c,
        };
        char::from_u32(code).unwrap_or(c)
    }
}

/// Set every letter and digit of `math` in `font`. The styled letters are upright symbols:
/// the style is in the character itself.
fn restyle(math: Math, font: Font) -> Math {
    let map = |text: String| text.chars().map(|c| font.apply(c)).collect::<String>();
    match math {
        Math::Row(items) => Math::Row(items.into_iter().map(|item| restyle(item, font)).collect()),
        Math::Ident(text) | Math::Number(text) | Math::Symbol(text) => Math::Symbol(map(text)),
        Math::Text(text) => Math::Symbol(map(text)),
        Math::Scripts { base, sub, sup } => {
            Math::Scripts { base: Box::new(restyle(*base, font)), sub, sup }
        }
        other => other,
    }
}

const SUPERSCRIPTS: &[(char, char)] = &[
    ('0', '⁰'),
    ('1', '¹'),
    ('2', '²'),
    ('3', '³'),
    ('4', '⁴'),
    ('5', '⁵'),
    ('6', '⁶'),
    ('7', '⁷'),
    ('8', '⁸'),
    ('9', '⁹'),
    ('+', '⁺'),
    ('−', '⁻'),
    ('-', '⁻'),
    ('=', '⁼'),
    ('(', '⁽'),
    (')', '⁾'),
    ('a', 'ᵃ'),
    ('b', 'ᵇ'),
    ('c', 'ᶜ'),
    ('d', 'ᵈ'),
    ('e', 'ᵉ'),
    ('f', 'ᶠ'),
    ('g', 'ᵍ'),
    ('h', 'ʰ'),
    ('i', 'ⁱ'),
    ('j', 'ʲ'),
    ('k', 'ᵏ'),
    ('l', 'ˡ'),
    ('m', 'ᵐ'),
    ('n', 'ⁿ'),
    ('o', 'ᵒ'),
    ('p', 'ᵖ'),
    ('r', 'ʳ'),
    ('s', 'ˢ'),
    ('t', 'ᵗ'),
    ('u', 'ᵘ'),
    ('v', 'ᵛ'),
    ('w', 'ʷ'),
    ('x', 'ˣ'),
    ('y', 'ʸ'),
    ('z', 'ᶻ'),
    ('T', 'ᵀ'),
    ('β', 'ᵝ'),
    ('γ', 'ᵞ'),
    ('δ', 'ᵟ'),
    ('φ', 'ᵠ'),
    ('χ', 'ᵡ'),
    ('′', '′'),
    ('∗', '*'),
];

const SUBSCRIPTS: &[(char, char)] = &[
    ('0', '₀'),
    ('1', '₁'),
    ('2', '₂'),
    ('3', '₃'),
    ('4', '₄'),
    ('5', '₅'),
    ('6', '₆'),
    ('7', '₇'),
    ('8', '₈'),
    ('9', '₉'),
    ('+', '₊'),
    ('−', '₋'),
    ('-', '₋'),
    ('=', '₌'),
    ('(', '₍'),
    (')', '₎'),
    ('a', 'ₐ'),
    ('e', 'ₑ'),
    ('h', 'ₕ'),
    ('i', 'ᵢ'),
    ('j', 'ⱼ'),
    ('k', 'ₖ'),
    ('l', 'ₗ'),
    ('m', 'ₘ'),
    ('n', 'ₙ'),
    ('o', 'ₒ'),
    ('p', 'ₚ'),
    ('r', 'ᵣ'),
    ('s', 'ₛ'),
    ('t', 'ₜ'),
    ('u', 'ᵤ'),
    ('v', 'ᵥ'),
    ('x', 'ₓ'),
    ('β', 'ᵦ'),
    ('γ', 'ᵧ'),
    ('ρ', 'ᵨ'),
    ('φ', 'ᵩ'),
    ('χ', 'ᵪ'),
];

/// `text` in the small raised or lowered characters, if every character has one.
fn script_chars(text: &str, table: &[(char, char)]) -> Option<String> {
    text.chars()
        .filter(|c| *c != ' ')
        .map(|c| table.iter().find(|(from, _)| *from == c).map(|(_, to)| *to))
        .collect()
}

impl Math {
    /// The text of a leaf: an identifier, number, symbol or operator.
    fn text(&self) -> Option<String> {
        match self {
            Math::Ident(text) | Math::Number(text) | Math::Symbol(text) | Math::Operator(text) => {
                Some(text.clone())
            }
            _ => None,
        }
    }

    /// Whether this is one glyph or a run that reads as one unit, so it needs no parentheses
    /// when flattened: `x`, `12`, `∞`, `x²`.
    fn is_atomic(&self) -> bool {
        match self {
            Math::Ident(_) | Math::Number(_) | Math::Symbol(_) | Math::Function { .. } => true,
            Math::LargeOp { .. } => true,
            Math::Fenced { .. } | Math::Table { .. } => true,
            Math::Accent { body, .. } => body.is_atomic(),
            Math::Scripts { base, .. } => base.is_atomic(),
            Math::Row(items) => items.is_empty() || (items.len() == 1 && items[0].is_atomic()),
            _ => false,
        }
    }

    /// How many lines of text this stands tall, roughly: a fraction is two. Frontends size
    /// growing delimiters by it.
    pub fn height(&self) -> f32 {
        match self {
            Math::Row(items) => items.iter().map(Math::height).fold(1., f32::max),
            Math::Frac { numerator, denominator, .. } => {
                0.8 * (numerator.height() + denominator.height())
            }
            Math::Sqrt { radicand, .. } => radicand.height() + 0.1,
            Math::Scripts { base, sub, sup } => {
                base.height()
                    + sub.as_ref().map_or(0., |s| 0.4 * s.height())
                    + sup.as_ref().map_or(0., |s| 0.4 * s.height())
            }
            Math::Fenced { body, .. } => body.height(),
            Math::Table { rows, .. } => {
                rows.iter().map(|row| row.iter().map(Math::height).fold(1., f32::max)).sum::<f32>()
            }
            Math::Accent { body, .. } => body.height() + 0.2,
            Math::LargeOp { .. } => 1.4,
            _ => 1.,
        }
    }

    /// The formula as one line of plain Unicode, for a frontend that can only draw text and
    /// for inline maths in running text: `x² + y² = r²`, `(a+b)/2`, `√(x+1)`.
    pub fn to_unicode(&self) -> String {
        let mut out = String::new();
        self.write_unicode(&mut out);
        out.trim().to_owned()
    }

    fn write_unicode(&self, out: &mut String) {
        match self {
            Math::Row(items) => {
                for item in items {
                    item.write_unicode(out);
                }
            }
            Math::Ident(text) | Math::Number(text) | Math::Symbol(text) => out.push_str(text),
            Math::Operator(text) if text == "," => out.push_str(", "),
            Math::Operator(text) => {
                // A sign at the start, or after another operator, is unary.
                let unary = out.trim_end().is_empty()
                    || out.trim_end().ends_with(['(', '[', '{', '=', '+', '−', ',']);
                if unary && (text == "−" || text == "+" || text == "±") {
                    out.push_str(text);
                } else {
                    if !out.ends_with(' ') && !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(text);
                    out.push(' ');
                }
            }
            Math::Function { name, .. } => {
                out.push_str(name);
                out.push('\u{2009}');
            }
            Math::LargeOp { symbol, .. } => out.push_str(symbol),
            Math::Text(text) => out.push_str(text),
            Math::Frac { numerator, denominator, line } => {
                let separator = if *line { "/" } else { " over " };
                numerator.write_wrapped(out);
                out.push_str(separator);
                denominator.write_wrapped(out);
            }
            Math::Sqrt { index, radicand } => {
                match index.as_deref().map(Math::to_unicode).as_deref() {
                    None | Some("2") => out.push('√'),
                    Some("3") => out.push('∛'),
                    Some("4") => out.push('∜'),
                    Some(index) => {
                        out.push_str(&script_chars(index, SUPERSCRIPTS).unwrap_or(index.into()));
                        out.push('√');
                    }
                }
                radicand.write_wrapped(out);
            }
            Math::Scripts { base, sub, sup } => {
                base.write_wrapped(out);
                // A large operator's limits read as `∑_{i=1}^{n}`, nothing smaller fits.
                for (script, table, mark) in [(sub, SUBSCRIPTS, '_'), (sup, SUPERSCRIPTS, '^')] {
                    let Some(script) = script else { continue };
                    let text = script.to_unicode();
                    match script_chars(&text, table) {
                        Some(small) if !small.is_empty() => out.push_str(&small),
                        _ => {
                            out.push(mark);
                            if text.chars().count() == 1 {
                                out.push_str(&text);
                            } else {
                                out.push('(');
                                out.push_str(&text);
                                out.push(')');
                            }
                        }
                    }
                }
            }
            Math::Fenced { open, close, body } => {
                out.push_str(open);
                body.write_unicode(out);
                out.push_str(close);
            }
            Math::Table { rows, open, close, .. } => {
                out.push_str(open);
                let rows: Vec<String> = rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(Math::to_unicode)
                            .filter(|cell| !cell.is_empty())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .collect();
                out.push_str(&rows.join("; "));
                out.push_str(close);
            }
            Math::Accent { accent, body } => {
                let text = body.to_unicode();
                match accent {
                    Accent::Overline | Accent::Underline => {
                        for c in text.chars() {
                            out.push(c);
                            out.push(accent.combining());
                        }
                    }
                    _ => {
                        out.push_str(&text);
                        out.push(accent.combining());
                    }
                }
            }
            Math::Space(width) if *width >= 0.25 => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            Math::Space(width) if *width > 0. => out.push('\u{2009}'),
            Math::Space(_) => {}
            Math::Unknown(raw) => out.push_str(raw),
        }
    }

    /// Flattened, in parentheses unless it reads as one unit.
    fn write_wrapped(&self, out: &mut String) {
        if self.is_atomic() {
            self.write_unicode(out);
        } else {
            out.push('(');
            out.push_str(&self.to_unicode());
            out.push(')');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(source: &str) -> String {
        parse(source).to_unicode()
    }

    #[test]
    fn letters_numbers_and_operators() {
        assert_eq!(flat("x + 2y = 10"), "x + 2y = 10");
        assert_eq!(flat("a - b"), "a − b");
        assert_eq!(flat("-x"), "−x");
        assert_eq!(flat(r"a \leq b \neq c"), "a ≤ b ≠ c");
        assert_eq!(parse("x"), Math::Ident("x".into()));
        assert_eq!(parse("3.14"), Math::Number("3.14".into()));
    }

    #[test]
    fn scripts_use_small_characters_where_they_exist() {
        assert_eq!(flat("x^2 + y^2 = r^2"), "x² + y² = r²");
        assert_eq!(flat("a_{i+1}"), "aᵢ₊₁");
        assert_eq!(flat("e^{i\\pi}"), "e^(iπ)");
        assert_eq!(flat("f'(x)"), "f′(x)");
        assert_eq!(flat("x_i^2"), "xᵢ²");
    }

    #[test]
    fn scripts_attach_to_their_base() {
        let Math::Scripts { base, sub, sup } = parse("x_1^2") else { panic!() };
        assert_eq!(*base, Math::Ident("x".into()));
        assert_eq!(sub.as_deref(), Some(&Math::Number("1".into())));
        assert_eq!(sup.as_deref(), Some(&Math::Number("2".into())));
    }

    #[test]
    fn fractions_and_roots() {
        assert_eq!(flat(r"\frac{a+b}{2}"), "(a + b)/2");
        assert_eq!(flat(r"\frac12"), "1/2");
        assert_eq!(flat(r"\sqrt{x+1}"), "√(x + 1)");
        assert_eq!(flat(r"\sqrt[3]{x}"), "∛x");
        let Math::Frac { numerator, denominator, line: true } = parse(r"\frac{1}{n}") else {
            panic!()
        };
        assert_eq!(*numerator, Math::Number("1".into()));
        assert_eq!(*denominator, Math::Ident("n".into()));
    }

    #[test]
    fn named_symbols_and_functions() {
        assert_eq!(flat(r"\alpha\beta\Gamma"), "αβΓ");
        assert_eq!(flat(r"\sin x"), "sin\u{2009}x");
        assert_eq!(flat(r"\sum_{i=1}^{n} i"), "∑ᵢ₌₁ⁿi");
        assert_eq!(flat(r"\mathbb{R}^n"), "ℝⁿ");
        assert_eq!(flat(r"\mathbf{v}"), "𝐯");
        assert!(matches!(parse(r"\lim_{x \to 0}"), Math::Scripts { .. }));
    }

    #[test]
    fn delimiters_and_matrices() {
        assert_eq!(flat(r"\left( \frac{a}{b} \right)"), "(a/b)");
        assert_eq!(flat(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}"), "(a, b; c, d)");
        let Math::Table { rows, open, align, .. } =
            parse(r"f(x) = \begin{cases} 1 & x > 0 \\ 0 & \text{otherwise} \end{cases}")
                .row_item(5)
        else {
            panic!()
        };
        assert_eq!(open, "{");
        assert_eq!(align, CellAlign::Left);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][1], Math::Text("otherwise".into()));
    }

    #[test]
    fn aligned_lines_without_an_environment() {
        let Math::Table { rows, align: CellAlign::Aligned, .. } = parse(r"a &= b \\ &= c") else {
            panic!()
        };
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn accents_and_text() {
        assert_eq!(flat(r"\hat{x}"), "x\u{302}");
        assert_eq!(flat(r"\text{if } x"), "if x");
    }

    #[test]
    fn what_it_does_not_know_stays_as_written() {
        assert_eq!(flat(r"\foo{x}"), r"\foox");
        // Unbalanced input still comes back.
        assert_eq!(flat(r"\frac{a"), "a/");
        assert_eq!(flat("}x{"), "x");
        assert_eq!(flat(r"x^"), "x");
        assert_eq!(flat(""), "");
    }

    impl Math {
        fn row_item(self, index: usize) -> Math {
            match self {
                Math::Row(mut items) => items.swap_remove(index),
                other => other,
            }
        }
    }
}
