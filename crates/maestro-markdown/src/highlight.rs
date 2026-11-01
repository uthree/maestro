//! Syntax highlighting for code blocks, as tokens that say what each piece of code is rather
//! than what colour to draw it: a frontend picks the colours from its own theme.

use std::sync::OnceLock;

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

/// What a piece of code is, for colouring.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenClass {
    /// Nothing in particular.
    Plain,
    /// A comment.
    Comment,
    /// A language keyword or storage word: `fn`, `let`, `if`.
    Keyword,
    /// A string or character literal.
    String,
    /// A number.
    Number,
    /// A named constant or literal word: `true`, `None`, `NULL`.
    Constant,
    /// A function's name, where it is defined or called.
    Function,
    /// A type, class or trait.
    Type,
    /// An operator: `+`, `=>`, `&&`.
    Operator,
    /// Brackets, separators and the like.
    Punctuation,
    /// An attribute, annotation, decorator or macro.
    Attribute,
    /// A markup tag: `<div>`.
    Tag,
    /// A line a diff adds.
    Inserted,
    /// A line a diff removes.
    Deleted,
    /// A heading in markup.
    Heading,
}

/// A run of code of one class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    /// The code, without line breaks.
    pub text: String,
    /// What it is.
    pub class: TokenClass,
}

/// Longer blocks are shown plain past this line, so one huge paste cannot stall a frame.
const MAX_LINES: usize = 2_000;
/// Lines longer than this are shown plain: minified code defeats the grammars.
const MAX_LINE: usize = 1_000;

fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// The grammar for a fence's language: a name (`rust`), an alias (`sh`) or an extension
/// (`rs`).
fn syntax_for(language: &str) -> Option<&'static SyntaxReference> {
    let set = syntaxes();
    let language = language.trim().to_ascii_lowercase();
    let language = match language.as_str() {
        // Close enough where the bundled grammars have no exact match.
        "ts" | "typescript" | "tsx" | "jsx" | "mjs" | "cjs" | "javascriptreact" => "js",
        "shell" | "zsh" | "console" | "shellsession" | "sh" | "fish" => "bash",
        "jsonc" | "json5" => "json",
        "py3" | "python3" => "python",
        "c++" | "hpp" | "cc" => "cpp",
        "objective-c" | "objc" => "m",
        "golang" => "go",
        "yml" => "yaml",
        "kt" | "kotlin" => "java",
        "patch" => "diff",
        "html5" | "xhtml" | "vue" | "svelte" => "html",
        other => other,
    };
    set.find_syntax_by_token(language)
}

/// The class a stack of scopes reads as. Comments and strings win over anything nested in
/// them (and a diff's added and removed lines over their `+` and `-`), so their quotes and
/// delimiters keep their colour; otherwise the innermost scope that
/// means something decides.
fn classify(stack: &[Scope]) -> TokenClass {
    let names: Vec<String> = stack.iter().map(|scope| scope.build_string()).collect();
    for name in &names {
        if name.starts_with("comment") {
            return TokenClass::Comment;
        }
        if name.starts_with("string") {
            return TokenClass::String;
        }
        if name.starts_with("markup.inserted") {
            return TokenClass::Inserted;
        }
        if name.starts_with("markup.deleted") {
            return TokenClass::Deleted;
        }
    }
    for name in names.iter().rev() {
        let class = match name.as_str() {
            n if n.starts_with("constant.numeric") => TokenClass::Number,
            n if n.starts_with("constant.character") => TokenClass::String,
            n if n.starts_with("constant") || n.starts_with("support.constant") => {
                TokenClass::Constant
            }
            n if n.starts_with("keyword.operator") => TokenClass::Operator,
            n if n.starts_with("keyword") || n.starts_with("storage") => TokenClass::Keyword,
            n if n.starts_with("entity.name.function")
                || n.starts_with("support.function")
                || n.starts_with("variable.function")
                || n.starts_with("meta.function-call.identifier") =>
            {
                TokenClass::Function
            }
            n if n.starts_with("entity.name.tag") => TokenClass::Tag,
            n if n.starts_with("entity.other.attribute-name") => TokenClass::Attribute,
            n if n.starts_with("entity.name.type")
                || n.starts_with("entity.name.class")
                || n.starts_with("entity.name.struct")
                || n.starts_with("entity.name.enum")
                || n.starts_with("entity.name.trait")
                || n.starts_with("entity.name.interface")
                || n.starts_with("entity.other.inherited-class")
                || n.starts_with("support.type")
                || n.starts_with("support.class") =>
            {
                TokenClass::Type
            }
            n if n.starts_with("entity.name.macro")
                || n.starts_with("support.macro")
                || n.starts_with("meta.annotation")
                || n.starts_with("meta.decorator")
                || n.starts_with("meta.attribute") =>
            {
                TokenClass::Attribute
            }
            n if n.starts_with("markup.heading") || n.starts_with("meta.diff.range") => {
                TokenClass::Heading
            }
            n if n.starts_with("punctuation") => TokenClass::Punctuation,
            _ => continue,
        };
        return class;
    }
    TokenClass::Plain
}

/// Add `text` to `line`, joining it to the last token when the class is the same.
fn push(line: &mut Vec<Token>, text: &str, class: TokenClass) {
    let text = text.trim_end_matches(['\n', '\r']);
    if text.is_empty() {
        return;
    }
    match line.last_mut() {
        Some(last) if last.class == class => last.text.push_str(text),
        _ => line.push(Token { text: text.to_owned(), class }),
    }
}

/// Split `code` into lines of tokens, read with the grammar for `language`. Code in a
/// language there is no grammar for comes back as plain lines.
pub fn highlight(code: &str, language: Option<&str>) -> Vec<Vec<Token>> {
    let plain = |line: &str| {
        let mut tokens = Vec::new();
        push(&mut tokens, line, TokenClass::Plain);
        tokens
    };
    let Some(syntax) = language.and_then(syntax_for) else {
        return code.lines().map(plain).collect();
    };
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut lines = Vec::new();
    for (index, line) in LinesWithEndings::from(code).enumerate() {
        if index >= MAX_LINES || line.len() > MAX_LINE {
            lines.push(plain(line));
            continue;
        }
        let Ok(ops) = state.parse_line(line, syntaxes()) else {
            lines.push(plain(line));
            continue;
        };
        let mut tokens = Vec::new();
        let mut at = 0;
        for (offset, op) in ops {
            if offset > at {
                push(&mut tokens, &line[at..offset], classify(stack.as_slice()));
                at = offset;
            }
            if stack.apply(&op).is_err() {
                break;
            }
        }
        push(&mut tokens, &line[at..], classify(stack.as_slice()));
        lines.push(tokens);
    }
    lines
}

/// Whether there is a grammar for `language`.
pub fn knows(language: &str) -> bool {
    syntax_for(language).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class_of(lines: &[Vec<Token>], text: &str) -> Option<TokenClass> {
        lines.iter().flatten().find(|token| token.text.trim() == text).map(|token| token.class)
    }

    #[test]
    fn rust_reads_as_rust() {
        let lines = highlight("fn main() {\n    // hi\n    let x = \"s\";\n}", Some("rust"));
        assert_eq!(lines.len(), 4);
        assert_eq!(class_of(&lines, "fn"), Some(TokenClass::Keyword));
        assert_eq!(class_of(&lines, "main"), Some(TokenClass::Function));
        assert_eq!(class_of(&lines, "// hi"), Some(TokenClass::Comment));
        assert_eq!(class_of(&lines, "\"s\""), Some(TokenClass::String));
        // Nothing is lost.
        let text: Vec<String> =
            lines.iter().map(|line| line.iter().map(|t| t.text.as_str()).collect()).collect();
        assert_eq!(text.join("\n"), "fn main() {\n    // hi\n    let x = \"s\";\n}");
    }

    #[test]
    fn aliases_find_a_grammar() {
        assert!(knows("sh"));
        assert!(knows("py"));
        assert!(knows("ts"));
        assert!(knows("JSON"));
        assert!(!knows("no-such-language"));
    }

    #[test]
    fn unknown_languages_come_back_plain() {
        let lines = highlight("a b\n\nc", Some("no-such-language"));
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], vec![Token { text: "a b".into(), class: TokenClass::Plain }]);
        assert!(lines[1].is_empty());
        assert_eq!(highlight("x", None)[0][0].class, TokenClass::Plain);
    }

    #[test]
    fn diffs_mark_their_lines() {
        let lines = highlight("+added\n-removed", Some("diff"));
        assert_eq!(lines[0][0].class, TokenClass::Inserted);
        assert_eq!(lines[1][0].class, TokenClass::Deleted);
    }
}
