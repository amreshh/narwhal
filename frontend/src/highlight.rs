//! Syntax highlighting for text previews.
//!
//! A small lexer per family of languages marks comments, strings, numbers,
//! keywords and the like. It doesn't parse: it only has to look right, never
//! fail on odd or cut-off input (previews stop mid-file), and stay small,
//! since the whole app is one WASM download.

use crate::util::ext;

/// What a piece of text is. Each has a colour in style.css (`.hl-…`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Comment,
    String,
    /// Numbers and literals (true, null, …).
    Constant,
    Keyword,
    Type,
    Function,
    /// Preprocessor lines, attributes, decorators, doctypes, fences.
    Meta,
    /// Keys, attribute names, properties and variables.
    Key,
    /// Markup tags.
    Tag,
    Heading,
    Inserted,
    Deleted,
}

impl Class {
    fn css(self) -> &'static str {
        match self {
            Class::Comment => "hl-c",
            Class::String => "hl-s",
            Class::Constant => "hl-n",
            Class::Keyword => "hl-k",
            Class::Type => "hl-t",
            Class::Function => "hl-f",
            Class::Meta => "hl-m",
            Class::Key => "hl-a",
            Class::Tag => "hl-g",
            Class::Heading => "hl-h",
            Class::Inserted => "hl-i",
            Class::Deleted => "hl-d",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Lang {
    Code(&'static Syntax),
    /// INI, TOML, .properties, .env, systemd units, …
    Config,
    Yaml,
    /// HTML and XML, with JavaScript and CSS inside.
    Markup,
    Markdown,
    Diff,
}

/// The language of a file, by its name; `None` for plain text.
pub fn lang_for(name: &str) -> Option<Lang> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "dockerfile" | "containerfile" => return Some(Lang::Code(&DOCKER)),
        "makefile" | "gnumakefile" | "justfile" | "pkgbuild" | ".bashrc" | ".zshrc" | ".profile" | ".bash_profile"
        | ".bash_aliases" => return Some(Lang::Code(&SHELL)),
        "gemfile" | "rakefile" | "vagrantfile" => return Some(Lang::Code(&RUBY)),
        ".gitignore" | ".dockerignore" | ".gitattributes" | "codeowners" => return Some(Lang::Code(&HASH)),
        ".editorconfig" | ".gitmodules" | ".gitconfig" | ".npmrc" | ".env" | "cargo.lock" => return Some(Lang::Config),
        _ => {}
    }
    if lower.starts_with("dockerfile.") || lower.starts_with("containerfile.") {
        return Some(Lang::Code(&DOCKER));
    }
    if lower.starts_with(".env.") {
        return Some(Lang::Config);
    }
    Some(match ext(name).as_str() {
        "rs" => Lang::Code(&RUST),
        "c" | "h" | "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" | "ino" | "java" | "cs" | "kt" | "kts" | "swift"
        | "scala" | "dart" | "groovy" | "gradle" | "php" | "zig" | "proto" => Lang::Code(&C_LIKE),
        "go" => Lang::Code(&GO),
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" => Lang::Code(&JS),
        "py" | "pyw" | "pyi" => Lang::Code(&PYTHON),
        "rb" | "rake" | "gemspec" => Lang::Code(&RUBY),
        "sh" | "bash" | "zsh" | "ksh" | "fish" | "mk" | "ps1" | "psm1" => Lang::Code(&SHELL),
        "dockerfile" | "containerfile" => Lang::Code(&DOCKER),
        "sql" => Lang::Code(&SQL),
        "lua" => Lang::Code(&LUA),
        "css" => Lang::Code(&CSS),
        "scss" | "sass" | "less" => Lang::Code(&SCSS),
        "json" | "jsonc" | "json5" | "ndjson" | "jsonl" | "geojson" | "webmanifest" => Lang::Code(&JSON),
        "tex" | "sty" | "cls" | "bib" => Lang::Code(&TEX),
        "r" | "pl" | "pm" | "ex" | "exs" | "cmake" | "nix" | "graphql" | "gql" | "gitignore" => Lang::Code(&HASH),
        "tf" | "tfvars" | "hcl" => Lang::Code(&HCL),
        "yaml" | "yml" => Lang::Yaml,
        "toml" | "ini" | "cfg" | "conf" | "properties" | "desktop" | "service" | "env" | "editorconfig" => Lang::Config,
        "html" | "htm" | "xhtml" | "xml" | "svg" | "vue" | "svelte" | "xsl" | "plist" => Lang::Markup,
        "md" | "markdown" | "mdx" => Lang::Markdown,
        "diff" | "patch" => Lang::Diff,
        _ => return None,
    })
}

/// The language of a Markdown code block, by its info string (```` ```rust ````).
fn lang_for_fence(info: &str) -> Option<Lang> {
    let lower = info.to_ascii_lowercase();
    let x = match lower.as_str() {
        "" | "text" | "plain" | "txt" => return None,
        "dockerfile" | "docker" | "containerfile" => return lang_for("Dockerfile"),
        "rust" => "rs",
        "javascript" | "node" => "js",
        "typescript" => "ts",
        "python" | "python3" => "py",
        "shell" | "console" | "shell-session" | "bash" | "zsh" => "sh",
        "golang" => "go",
        "ruby" => "rb",
        "csharp" | "c#" => "cs",
        "c++" => "cpp",
        "kotlin" => "kt",
        "terraform" => "tf",
        other => other,
    };
    lang_for(&format!("x.{x}"))
}

/// The text as HTML, highlighted parts in `<span class="hl-…">`. Everything
/// else is escaped, so the result is safe to use as inner HTML.
pub fn to_html(lang: Lang, text: &str) -> String {
    let mut spans = Spans::default();
    lex(lang, text, 0, text.len(), &mut spans);
    let mut spans = spans.0;
    spans.sort_by_key(|s| s.0);
    let mut html = String::with_capacity(text.len() + text.len() / 2);
    let mut pos = 0;
    for (start, end, class) in spans {
        // Never happens; but a panic would take the app down with it.
        if start < pos || end > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        escape(&mut html, &text[pos..start]);
        html.push_str("<span class=\"");
        html.push_str(class.css());
        html.push_str("\">");
        escape(&mut html, &text[start..end]);
        html.push_str("</span>");
        pos = end;
    }
    escape(&mut html, &text[pos..]);
    html
}

fn escape(html: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            c => html.push(c),
        }
    }
}

/// The highlighted parts: byte ranges of the text.
#[derive(Default)]
struct Spans(Vec<(usize, usize, Class)>);

impl Spans {
    fn add(&mut self, start: usize, end: usize, class: Class) {
        if start >= end {
            return;
        }
        match self.0.last_mut() {
            Some(last) if last.1 == start && last.2 == class => last.1 = end,
            _ => self.0.push((start, end, class)),
        }
    }
}

/// Highlight `text[from..to]`.
fn lex(lang: Lang, text: &str, from: usize, to: usize, out: &mut Spans) {
    match lang {
        Lang::Code(syn) => code(syn, text, from, to, out),
        Lang::Config => config(text, from, to, out),
        Lang::Yaml => yaml(text, from, to, out),
        Lang::Markup => markup(text, from, to, out),
        Lang::Markdown => markdown(text, from, to, out),
        Lang::Diff => diff(text, from, to, out),
    }
}

// ---------------------------------------------------------------------------
// Programming languages
// ---------------------------------------------------------------------------

/// How a programming language looks to the lexer.
#[derive(Debug)]
pub struct Syntax {
    /// Line comments.
    line: &'static [&'static str],
    /// Block comments: start and end.
    block: &'static [(&'static str, &'static str)],
    /// String delimiters, longest first (`"""` before `"`).
    quotes: &'static [&'static str],
    keywords: &'static [&'static str],
    /// Shown like numbers: true, false, null, …
    literals: &'static [&'static str],
    /// Built-in types with lowercase names (i32, string).
    types: &'static [&'static str],
    /// Keywords, literals and types in any case (SQL).
    any_case: bool,
    /// A character before a name that changes what it is: `$var`, `@decorator`.
    sigils: &'static [(u8, Class)],
    /// Capitalised names are types, all-caps ones constants.
    caps_types: bool,
    /// `#` only starts a comment at the start of a word (shell: `$#`, `a#b`).
    hash_word: bool,
    /// A line starting with `#` is a preprocessor line or attribute (C, Rust).
    hash_meta: bool,
    /// `'` only quotes one character: `'a` alone is a lifetime (Rust).
    char_quote: bool,
    /// `name!(…)` is a macro call (Rust).
    macros: bool,
    /// Strings in quotes of one character can span lines (shell).
    multiline: bool,
    /// A string followed by `:` is a key (JSON).
    string_keys: bool,
    /// A name followed by `=` is a key or variable (shell, HCL).
    eq_keys: bool,
    /// CSS: selectors, properties, `#fff` colours, `-` inside names.
    css: bool,
}

const C_BLOCK: &[(&str, &str)] = &[("/*", "*/")];

const BASE: Syntax = Syntax {
    line: &[],
    block: &[],
    quotes: &["\"", "'"],
    keywords: &[],
    literals: &[],
    types: &[],
    any_case: false,
    sigils: &[],
    caps_types: false,
    hash_word: false,
    hash_meta: false,
    char_quote: false,
    macros: false,
    multiline: false,
    string_keys: false,
    eq_keys: false,
    css: false,
};

static RUST: Syntax = Syntax {
    line: &["//"],
    block: C_BLOCK,
    quotes: &["\""],
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "fn", "for",
        "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "static", "struct",
        "super", "trait", "type", "unsafe", "use", "where", "while", "yield",
    ],
    literals: &["true", "false", "self"],
    types: &[
        "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "str", "u8", "u16", "u32", "u64",
        "u128", "usize",
    ],
    caps_types: true,
    hash_meta: true,
    char_quote: true,
    macros: true,
    ..BASE
};

/// C, C++, Java, C#, Kotlin, Swift, Scala, Dart, Groovy, PHP, Zig: close
/// enough to share their keywords.
static C_LIKE: Syntax = Syntax {
    line: &["//"],
    block: C_BLOCK,
    quotes: &["\"\"\"", "\"", "'"],
    keywords: &[
        "abstract", "as", "async", "auto", "await", "bool", "boolean", "break", "byte", "case", "catch", "char",
        "class", "const", "constexpr", "continue", "def", "default", "defer", "delete", "do", "double", "echo", "else",
        "enum", "explicit", "export", "extends", "extern", "final", "finally", "float", "fn", "for", "foreach", "fun",
        "func", "function", "goto", "guard", "if", "impl", "implements", "import", "in", "inline", "instanceof", "int",
        "interface", "internal", "is", "let", "long", "namespace", "new", "operator", "override", "package", "private",
        "protected", "public", "readonly", "register", "return", "sealed", "short", "signed", "sizeof", "static",
        "struct", "super", "switch", "synchronized", "template", "throw", "throws", "trait", "try", "typedef",
        "typename", "typeof", "union", "unsigned", "using", "val", "var", "virtual", "void", "volatile", "when",
        "where", "while", "yield",
    ],
    literals: &["true", "false", "null", "nullptr", "NULL", "nil", "this", "self"],
    sigils: &[(b'$', Class::Key), (b'@', Class::Meta)],
    caps_types: true,
    hash_meta: true,
    ..BASE
};

static GO: Syntax = Syntax {
    line: &["//"],
    block: C_BLOCK,
    quotes: &["\"", "'", "`"],
    keywords: &[
        "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for", "func", "go",
        "goto", "if", "import", "interface", "map", "package", "range", "return", "select", "struct", "switch", "type",
        "var",
    ],
    literals: &["true", "false", "nil", "iota"],
    types: &[
        "any", "bool", "byte", "error", "float32", "float64", "int", "int8", "int16", "int32", "int64", "rune",
        "string", "uint", "uint8", "uint16", "uint32", "uint64", "uintptr",
    ],
    ..BASE
};

/// JavaScript and TypeScript.
static JS: Syntax = Syntax {
    line: &["//"],
    block: C_BLOCK,
    quotes: &["\"", "'", "`"],
    keywords: &[
        "abstract", "as", "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger",
        "declare", "default", "delete", "do", "else", "enum", "export", "extends", "finally", "for", "from",
        "function", "if", "implements", "import", "in", "instanceof", "interface", "keyof", "let", "namespace", "new",
        "of", "private", "protected", "public", "readonly", "return", "satisfies", "static", "super", "switch",
        "throw", "try", "type", "typeof", "var", "void", "while", "with", "yield",
    ],
    literals: &["true", "false", "null", "undefined", "this", "NaN", "Infinity"],
    types: &["any", "bigint", "boolean", "never", "number", "object", "string", "symbol", "unknown"],
    sigils: &[(b'@', Class::Meta)],
    caps_types: true,
    ..BASE
};

static PYTHON: Syntax = Syntax {
    line: &["#"],
    quotes: &["\"\"\"", "'''", "\"", "'"],
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def", "del", "elif", "else",
        "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "match", "nonlocal", "not",
        "or", "pass", "raise", "return", "try", "while", "with", "yield",
    ],
    literals: &["True", "False", "None", "self", "cls"],
    sigils: &[(b'@', Class::Meta)],
    caps_types: true,
    ..BASE
};

static RUBY: Syntax = Syntax {
    line: &["#"],
    quotes: &["\"", "'", "`"],
    keywords: &[
        "alias",
        "and",
        "begin",
        "break",
        "case",
        "class",
        "def",
        "do",
        "else",
        "elsif",
        "end",
        "ensure",
        "for",
        "if",
        "in",
        "include",
        "module",
        "next",
        "not",
        "or",
        "private",
        "protected",
        "public",
        "raise",
        "redo",
        "require",
        "require_relative",
        "rescue",
        "retry",
        "return",
        "super",
        "then",
        "undef",
        "unless",
        "until",
        "when",
        "while",
        "yield",
    ],
    literals: &["true", "false", "nil", "self"],
    sigils: &[(b'@', Class::Key), (b'$', Class::Key), (b':', Class::Constant)],
    caps_types: true,
    multiline: true,
    ..BASE
};

static SHELL: Syntax = Syntax {
    line: &["#"],
    quotes: &["\"", "'", "`"],
    keywords: &[
        "alias", "break", "case", "continue", "declare", "do", "done", "elif", "else", "esac", "eval", "exec", "exit",
        "export", "fi", "for", "function", "if", "in", "local", "readonly", "return", "select", "set", "shift",
        "source", "then", "trap", "typeset", "unset", "until", "while",
    ],
    literals: &["true", "false"],
    sigils: &[(b'$', Class::Key)],
    hash_word: true,
    multiline: true,
    eq_keys: true,
    ..BASE
};

static DOCKER: Syntax = Syntax {
    keywords: &[
        "FROM", "AS", "RUN", "CMD", "LABEL", "MAINTAINER", "EXPOSE", "ENV", "ADD", "COPY", "ENTRYPOINT", "VOLUME",
        "USER", "WORKDIR", "ARG", "ONBUILD", "STOPSIGNAL", "HEALTHCHECK", "SHELL", "case", "do", "done", "elif",
        "else", "esac", "exit", "export", "fi", "for", "if", "in", "then", "while",
    ],
    multiline: false,
    ..SHELL
};

static SQL: Syntax = Syntax {
    line: &["--"],
    block: C_BLOCK,
    quotes: &["'", "\"", "`"],
    keywords: &[
        "add", "all", "alter", "and", "as", "asc", "begin", "between", "by", "cascade", "case", "check", "column",
        "commit", "constraint", "create", "cross", "database", "default", "delete", "desc", "distinct", "drop", "else",
        "end", "exists", "foreign", "from", "full", "grant", "group", "having", "if", "in", "index", "inner", "insert",
        "into", "is", "join", "key", "left", "like", "limit", "not", "offset", "on", "or", "order", "outer", "primary",
        "references", "replace", "returning", "revoke", "right", "rollback", "schema", "select", "set", "table",
        "then", "transaction", "trigger", "truncate", "union", "unique", "update", "using", "values", "view", "when",
        "where", "with",
    ],
    literals: &["null", "true", "false"],
    types: &[
        "bigint", "boolean", "char", "date", "decimal", "double", "float", "int", "integer", "numeric", "real",
        "serial", "smallint", "text", "timestamp", "uuid", "varchar",
    ],
    any_case: true,
    multiline: true,
    ..BASE
};

static LUA: Syntax = Syntax {
    line: &["--"],
    block: &[("--[[", "]]")],
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in", "local", "not", "or",
        "repeat", "return", "then", "until", "while",
    ],
    literals: &["nil", "true", "false", "self"],
    ..BASE
};

static CSS: Syntax =
    Syntax { block: C_BLOCK, sigils: &[(b'@', Class::Keyword), (b'$', Class::Key)], css: true, ..BASE };

/// SCSS, Sass and Less, which also have `//` comments.
static SCSS: Syntax = Syntax { line: &["//"], ..CSS };

static JSON: Syntax = Syntax {
    line: &["//"],
    block: C_BLOCK,
    quotes: &["\""],
    literals: &["true", "false", "null"],
    string_keys: true,
    ..BASE
};

static TEX: Syntax =
    Syntax { line: &["%"], quotes: &[], sigils: &[(b'\\', Class::Keyword), (b'@', Class::Keyword)], ..BASE };

/// Anything else with `#` comments: R, Perl, Elixir, CMake, Nix, GraphQL, …
static HASH: Syntax = Syntax {
    line: &["#"],
    literals: &["true", "false", "null", "nil", "TRUE", "FALSE", "NULL", "NA"],
    hash_word: true,
    eq_keys: true,
    ..BASE
};

/// Terraform and other HCL.
static HCL: Syntax = Syntax { line: &["#", "//"], block: C_BLOCK, quotes: &["\""], ..HASH };

/// Values in config files and YAML.
static VALUE: Syntax = Syntax {
    line: &["#"],
    literals: &["true", "false", "yes", "no", "on", "off", "null"],
    any_case: true,
    hash_word: true,
    ..BASE
};

static YAML_VALUE: Syntax =
    Syntax { sigils: &[(b'&', Class::Meta), (b'*', Class::Meta), (b'!', Class::Meta)], ..VALUE };

fn code(syn: &Syntax, text: &str, from: usize, to: usize, out: &mut Spans) {
    let b = &text.as_bytes()[..to];
    let n = to;
    let mut i = from;
    let mut line_start = true;
    // CSS: the next `{`, `;` or `}` and where it is. A name before a `{` is
    // part of a selector, before the others part of a declaration.
    let mut css_end: Option<(usize, u8)> = None;
    // CSS: inside parentheses, as in @media (max-width: 600px).
    let mut parens = 0u32;
    while i < n {
        let c = b[i];
        if c == b'\n' {
            line_start = true;
            i += 1;
            continue;
        }
        if matches!(c, b' ' | b'\t' | b'\r') {
            i += 1;
            continue;
        }
        let first = std::mem::replace(&mut line_start, false);
        let prev = if i > from { b[i - 1] } else { b'\n' };
        let rest = &b[i..];

        // Comments.
        if let Some(&(open, close)) = syn.block.iter().find(|(o, _)| rest.starts_with(o.as_bytes())) {
            let end = find(b, i + open.len(), close).map_or(n, |e| e + close.len());
            out.add(i, end, Class::Comment);
            i = end;
            continue;
        }
        if syn.line.iter().any(|l| rest.starts_with(l.as_bytes()))
            && !(syn.hash_word && c == b'#' && !prev.is_ascii_whitespace())
        {
            let end = line_end(b, i);
            out.add(i, end, Class::Comment);
            i = end;
            continue;
        }
        if syn.hash_meta && first && c == b'#' {
            let end = line_end(b, i);
            out.add(i, end, Class::Meta);
            i = end;
            continue;
        }

        // Strings.
        if c == b'\'' && syn.char_quote {
            let char_end = if b.get(i + 1) == Some(&b'\\') {
                find(b, i + 3, "'").filter(|&e| e < i + 14).map(|e| e + 1)
            } else {
                let len = utf8_len(b.get(i + 1).copied().unwrap_or(b' '));
                (b.get(i + 1 + len) == Some(&b'\'')).then_some(i + 2 + len)
            };
            if let Some(end) = char_end {
                out.add(i, end, Class::String);
                i = end;
            } else {
                // A lifetime: 'a, 'static.
                let end = ident_end(b, i + 1, syn);
                out.add(i, end, Class::Meta);
                i = end.max(i + 1);
            }
            continue;
        }
        if let Some(q) = syn.quotes.iter().find(|q| rest.starts_with(q.as_bytes())) {
            if *q != "'" || apostrophe_quotes(b, i, from) {
                let end = string_end(b, i, q, syn.multiline || q.len() > 1 || *q == "`");
                let class = if syn.string_keys && b.get(skip_blanks(b, end)) == Some(&b':') {
                    Class::Key
                } else {
                    Class::String
                };
                out.add(i, end, class);
                i = end;
                continue;
            }
        }

        // Numbers: 42, 0xff, 1.5e3, 10px, 1_000u32.
        if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit) && !is_word(prev)) {
            let mut j = i + 1;
            while j < n && (is_word(b[j]) || (b[j] == b'.' && b.get(j + 1).is_some_and(u8::is_ascii_digit))) {
                j += 1;
            }
            out.add(i, j, Class::Constant);
            i = j;
            continue;
        }

        // Sigils: $var, ${var}, @decorator, :symbol, \command.
        if let Some(&(_, class)) = syn.sigils.iter().find(|(s, _)| *s == c) {
            let next = b.get(i + 1).copied().unwrap_or(b'\n');
            let end = if c == b'$' && next == b'{' {
                Some(find(b, i + 2, "}").filter(|&e| e < line_end(b, i)).map_or(line_end(b, i), |e| e + 1))
            } else if is_word_start(next) {
                (c != b':' || (prev != b':' && !is_word(prev))).then(|| ident_end(b, i + 1, syn))
            } else if c == b'$' && (next.is_ascii_digit() || b"@#?*!$-".contains(&next)) {
                Some(i + 2)
            } else if c == b'\\' && next != b'\n' {
                Some((i + 1 + utf8_len(next)).min(n))
            } else {
                None
            };
            if let Some(end) = end {
                out.add(i, end, class);
                i = end;
                continue;
            }
        }

        // CSS selectors and colours: .class, #id, #fff.
        let mut selector = false;
        if syn.css {
            if css_end.is_none_or(|(at, _)| at < i) {
                let next = b[i..].iter().position(|x| b"{;}".contains(x));
                css_end = Some(next.map_or((n, b'}'), |p| (i + p, b[i + p])));
            }
            match c {
                b'(' => parens += 1,
                b')' => parens = parens.saturating_sub(1),
                _ => {}
            }
            selector = parens == 0 && css_end.is_some_and(|(_, x)| x == b'{');
            if (c == b'#' || c == b'.') && b.get(i + 1).is_some_and(|&x| is_word(x) || x == b'-') {
                let end = ident_end(b, i + 1, syn);
                out.add(i, end, if c == b'#' && !selector { Class::Constant } else { Class::Type });
                i = end;
                continue;
            }
        }

        // Names.
        if is_word_start(c) {
            let end = ident_end(b, i, syn);
            let word = &text[i..end];
            let next = b.get(end).copied();
            let after = skip_blanks(b, end);
            let mut stop = end;
            let class = if has(syn.keywords, word, syn.any_case) {
                Some(Class::Keyword)
            } else if has(syn.literals, word, syn.any_case) {
                Some(Class::Constant)
            } else if has(syn.types, word, syn.any_case) {
                Some(Class::Type)
            } else if next == Some(b'(') {
                Some(Class::Function)
            } else if syn.macros && next == Some(b'!') && b.get(end + 1) != Some(&b'=') {
                stop = end + 1;
                Some(Class::Function)
            } else if syn.css {
                if selector {
                    Some(Class::Tag)
                } else {
                    (b.get(after) == Some(&b':')).then_some(Class::Key)
                }
            } else if syn.eq_keys && b.get(after) == Some(&b'=') && b.get(after + 1) != Some(&b'=') {
                Some(Class::Key)
            } else if syn.caps_types && word.as_bytes()[0].is_ascii_uppercase() {
                if word.len() > 1 && !word.bytes().any(|x| x.is_ascii_lowercase()) {
                    Some(Class::Constant)
                } else {
                    Some(Class::Type)
                }
            } else {
                None
            };
            if let Some(class) = class {
                out.add(i, stop, class);
            }
            i = stop;
            continue;
        }

        // Punctuation, and characters beyond ASCII.
        i += utf8_len(c);
    }
}

fn has(list: &[&str], word: &str, any_case: bool) -> bool {
    list.iter().any(|k| if any_case { k.eq_ignore_ascii_case(word) } else { *k == word })
}

/// A `'` right after a word is an apostrophe (don't, it's), unless the word
/// is a string prefix: f'…', r'…', b'…', u8'…'.
fn apostrophe_quotes(b: &[u8], i: usize, from: usize) -> bool {
    let mut j = i;
    while j > from && is_word(b[j - 1]) {
        j -= 1;
    }
    let word = &b[j..i];
    if word.is_empty() {
        // After a letter beyond ASCII: café's.
        return !(j > from && b[j - 1] >= 0x80);
    }
    word.len() <= 2 && word.iter().all(|c| b"fFrRbBuUL8".contains(c))
}

/// Where the string starting at `i` with quote `q` ends (after its closing
/// quote), or the end of the line or text when it isn't closed.
fn string_end(b: &[u8], i: usize, q: &str, multiline: bool) -> usize {
    let q = q.as_bytes();
    let mut j = i + q.len();
    while j < b.len() {
        if b[j] == b'\\' {
            j += 2;
            continue;
        }
        if b[j..].starts_with(q) {
            return j + q.len();
        }
        if b[j] == b'\n' && !multiline {
            return j;
        }
        j += 1;
    }
    b.len()
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn is_word_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn ident_end(b: &[u8], mut i: usize, syn: &Syntax) -> usize {
    while i < b.len() && (is_word(b[i]) || (syn.css && b[i] == b'-')) {
        i += 1;
    }
    i
}

/// Bytes in the UTF-8 character that starts with `c`.
fn utf8_len(c: u8) -> usize {
    match c {
        0..0xC0 => 1,
        0xC0..0xE0 => 2,
        0xE0..0xF0 => 3,
        _ => 4,
    }
}

fn find(b: &[u8], from: usize, pat: &str) -> Option<usize> {
    let pat = pat.as_bytes();
    b.get(from..)?.windows(pat.len()).position(|w| w == pat).map(|p| p + from)
}

fn find_any_case(b: &[u8], from: usize, pat: &str) -> Option<usize> {
    let pat = pat.as_bytes();
    b.get(from..)?.windows(pat.len()).position(|w| w.eq_ignore_ascii_case(pat)).map(|p| p + from)
}

fn line_end(b: &[u8], i: usize) -> usize {
    b[i..].iter().position(|&x| x == b'\n').map_or(b.len(), |p| i + p)
}

fn skip_blanks(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    i
}

/// The lines of `text[from..to]`, with where each starts.
fn lines(text: &str, from: usize, to: usize) -> impl Iterator<Item = (usize, &str)> {
    text.get(from..to).unwrap_or("").split('\n').scan(from, |pos, line| {
        let start = *pos;
        *pos += line.len() + 1;
        Some((start, line.strip_suffix('\r').unwrap_or(line)))
    })
}

// ---------------------------------------------------------------------------
// Config files
// ---------------------------------------------------------------------------

/// INI, TOML and the like: `[sections]`, `key = value` and comments.
fn config(text: &str, from: usize, to: usize, out: &mut Spans) {
    for (start, line) in lines(text, from, to) {
        let t = line.trim_start();
        let s = start + line.len() - t.len();
        let e = start + line.len();
        if t.starts_with(['#', ';']) {
            out.add(s, e, Class::Comment);
            continue;
        }
        if t.starts_with('[') {
            out.add(s, s + t.trim_end().len(), Class::Heading);
            continue;
        }
        let mut value = s;
        if let Some(k) = t.find(['=', ':']) {
            let key = t[..k].trim_end();
            if !key.is_empty() && (!key.contains([' ', '\t']) || key.starts_with('"')) {
                out.add(s, s + key.len(), Class::Key);
                value = s + k + 1;
            }
        }
        code(&VALUE, text, value, e, out);
    }
}

fn yaml(text: &str, from: usize, to: usize, out: &mut Spans) {
    // In a block scalar (`key: |`): the indent of its key. Lines indented
    // deeper are its text.
    let mut block: Option<usize> = None;
    for (start, line) in lines(text, from, to) {
        let t = line.trim_start();
        let indent = line.len() - t.len();
        let s = start + indent;
        let e = start + line.len();
        if let Some(k) = block {
            if t.is_empty() || indent > k {
                out.add(s, e, Class::String);
                continue;
            }
            block = None;
        }
        if t.starts_with('#') {
            out.add(s, e, Class::Comment);
            continue;
        }
        if t.starts_with("---") || t.starts_with("...") {
            out.add(s, e, Class::Meta);
            continue;
        }
        // List items: "- ", "- - ".
        let mut p = s;
        while text[p..e].starts_with("- ") {
            p = skip_blanks(&text.as_bytes()[..e], p + 2);
        }
        match yaml_key(&text[p..e]) {
            Some(k) => {
                out.add(p, p + k, Class::Key);
                let v = text[p + k + 1..e].trim();
                if v.starts_with(['|', '>']) && v[1..].bytes().all(|x| b"+-0123456789".contains(&x)) {
                    block = Some(p - start);
                }
                code(&YAML_VALUE, text, p + k + 1, e, out);
            }
            None => code(&YAML_VALUE, text, p, e, out),
        }
    }
}

/// The length of the key, if the line is `key: value` or `key:`.
fn yaml_key(t: &str) -> Option<usize> {
    let b = t.as_bytes();
    let end = match *b.first()? {
        q @ (b'"' | b'\'') => 1 + t[1..].find(char::from(q))? + 1,
        b'{' | b'[' | b'&' | b'*' | b'!' | b'|' | b'>' | b'#' => return None,
        _ => {
            let k = t.find(':')?;
            if t[..k].contains(" #") {
                return None;
            }
            k
        }
    };
    (b.get(end) == Some(&b':') && matches!(b.get(end + 1), None | Some(b' ' | b'\t'))).then_some(end)
}

// ---------------------------------------------------------------------------
// HTML and XML
// ---------------------------------------------------------------------------

fn markup(text: &str, from: usize, to: usize, out: &mut Spans) {
    let b = &text.as_bytes()[..to];
    let mut i = from;
    while i < to {
        match b[i] {
            b'<' => i = tag(text, b, i, out),
            b'&' => {
                // &amp; &#39; &#x27;
                let end = b[i + 1..].iter().take(32).position(|&x| x == b';').map(|p| i + 1 + p);
                match end {
                    Some(e) if e > i + 1 && b[i + 1..e].iter().all(|x| x.is_ascii_alphanumeric() || *x == b'#') => {
                        out.add(i, e + 1, Class::Constant);
                        i = e + 1;
                    }
                    _ => i += 1,
                }
            }
            _ => i += 1,
        }
    }
}

/// The tag, comment, doctype or CDATA section at `i` (a `<`); returns where it ends.
fn tag(text: &str, b: &[u8], i: usize, out: &mut Spans) -> usize {
    let n = b.len();
    let rest = &b[i..];
    let through = |pat: &str, from: usize| find(b, from, pat).map_or(n, |e| e + pat.len());
    if rest.starts_with(b"<!--") {
        let end = through("-->", i + 4);
        out.add(i, end, Class::Comment);
        return end;
    }
    if rest.starts_with(b"<![CDATA[") {
        let end = through("]]>", i + 9);
        out.add(i, end, Class::String);
        return end;
    }
    if rest.starts_with(b"<!") || rest.starts_with(b"<?") {
        let end = through(">", i + 2);
        out.add(i, end, Class::Meta);
        return end;
    }
    let closing = rest.get(1) == Some(&b'/');
    let name_start = i + 1 + usize::from(closing);
    if !b.get(name_start).is_some_and(u8::is_ascii_alphabetic) {
        return i + 1;
    }
    let mut j = name_start;
    while j < n && (is_word(b[j]) || b"-:.".contains(&b[j])) {
        j += 1;
    }
    out.add(name_start, j, Class::Tag);
    let name = &text[name_start..j];

    // Attributes, up to the end of the tag.
    let mut self_closing = false;
    while j < n {
        match b[j] {
            b'>' => {
                j += 1;
                break;
            }
            // Broken markup: another tag starts.
            b'<' => break,
            b'/' => {
                self_closing = true;
                j += 1;
            }
            q @ (b'"' | b'\'') => {
                let end = find(b, j + 1, if q == b'"' { "\"" } else { "'" }).map_or(n, |e| e + 1);
                out.add(j, end, Class::String);
                j = end;
            }
            b'=' => {
                j = skip_blanks(b, j + 1);
                // An unquoted value.
                let start = j;
                while j < n && !b[j].is_ascii_whitespace() && !b"\"'<>".contains(&b[j]) {
                    j += 1;
                }
                out.add(start, j, Class::String);
            }
            x if x.is_ascii_whitespace() => j += 1,
            _ => {
                let start = j;
                while j < n && !b[j].is_ascii_whitespace() && !b"=/<>\"'".contains(&b[j]) {
                    j += 1;
                }
                out.add(start, j, Class::Key);
            }
        }
    }

    // JavaScript and CSS inside.
    if !closing && !self_closing {
        let inner = if name.eq_ignore_ascii_case("script") {
            Some((&JS, "</script"))
        } else if name.eq_ignore_ascii_case("style") {
            Some((&CSS, "</style"))
        } else {
            None
        };
        if let Some((syn, end_tag)) = inner {
            let close = find_any_case(b, j, end_tag).unwrap_or(n);
            code(syn, text, j, close, out);
            return close;
        }
    }
    j
}

// ---------------------------------------------------------------------------
// Markdown and diffs
// ---------------------------------------------------------------------------

fn markdown(text: &str, from: usize, to: usize, out: &mut Spans) {
    // In a fenced code block: its fence, language and where its code starts.
    let mut fence: Option<(&str, Option<Lang>, usize)> = None;
    for (start, line) in lines(text, from, to) {
        let t = line.trim_start();
        let s = start + line.len() - t.len();
        let e = start + line.len();
        if let Some((mark, lang, code_start)) = fence {
            if t.starts_with(mark) {
                if let Some(lang) = lang {
                    lex(lang, text, code_start, start.max(code_start), out);
                }
                out.add(s, e, Class::Meta);
                fence = None;
            }
            continue;
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            let info = t[3..].trim_start_matches(['`', '~']).trim();
            let word = info.split([' ', '{', ',']).next().unwrap_or("");
            fence = Some((&t[..3], lang_for_fence(word), (e + 1).min(to)));
            out.add(s, e, Class::Meta);
            continue;
        }
        let tb = t.as_bytes();
        let hashes = tb.iter().take_while(|&&x| x == b'#').count();
        if (1..=6).contains(&hashes) && matches!(tb.get(hashes), None | Some(b' ' | b'\t')) {
            out.add(s, e, Class::Heading);
            continue;
        }
        if t.starts_with('>') {
            out.add(s, e, Class::Comment);
            continue;
        }
        // Rules (---, ***) and setext heading underlines (===).
        if let Some(&first) = tb.first() {
            let same = tb.iter().filter(|&&x| x == first).count();
            if b"-*_=".contains(&first) && same >= 3 && tb.iter().all(|&x| x == first || x == b' ') {
                out.add(s, e, if first == b'=' { Class::Heading } else { Class::Meta });
                continue;
            }
        }
        // List markers: "- ", "* ", "1. ".
        let mut p = s;
        let digits = tb.iter().take_while(|x| x.is_ascii_digit()).count();
        if tb.first().is_some_and(|x| b"-*+".contains(x)) && matches!(tb.get(1), Some(b' ' | b'\t')) {
            p = s + 1;
        } else if (1..10).contains(&digits)
            && matches!(tb.get(digits), Some(b'.' | b')'))
            && matches!(tb.get(digits + 1), Some(b' ' | b'\t'))
        {
            p = s + digits + 1;
        }
        out.add(s, p, Class::Keyword);
        inline(text, p, e, out);
    }
    // Cut off inside a code block.
    if let Some((_, Some(lang), code_start)) = fence {
        lex(lang, text, code_start, to.max(code_start), out);
    }
}

/// `code` and link targets within a line of Markdown.
fn inline(text: &str, from: usize, to: usize, out: &mut Spans) {
    let b = &text.as_bytes()[..to];
    let mut i = from;
    while i < to {
        match b[i] {
            b'`' => {
                let ticks = b[i..].iter().take_while(|&&x| x == b'`').count();
                match find(b, i + ticks, &text[i..i + ticks]) {
                    Some(e) => {
                        out.add(i, e + ticks, Class::String);
                        i = e + ticks;
                    }
                    None => i += ticks,
                }
            }
            b']' if b.get(i + 1) == Some(&b'(') => match find(b, i + 2, ")") {
                Some(e) => {
                    out.add(i + 2, e, Class::Key);
                    i = e + 1;
                }
                None => i += 1,
            },
            _ => i += 1,
        }
    }
}

fn diff(text: &str, from: usize, to: usize, out: &mut Spans) {
    for (start, line) in lines(text, from, to) {
        let class = if ["+++", "---", "diff ", "index "].iter().any(|p| line.starts_with(p)) {
            Class::Meta
        } else if line.starts_with("@@") {
            Class::Function
        } else if line.starts_with('+') {
            Class::Inserted
        } else if line.starts_with('-') {
            Class::Deleted
        } else {
            continue;
        };
        out.add(start, start + line.len(), class);
    }
}

#[cfg(test)]
mod tests {
    use super::Class::*;
    use super::*;

    // `String` is Class::String here.
    fn marks(name: &str, text: &str) -> Vec<(std::string::String, Class)> {
        let mut out = Spans::default();
        lex(lang_for(name).expect(name), text, 0, text.len(), &mut out);
        out.0.iter().map(|&(s, e, c)| (text[s..e].to_string(), c)).collect()
    }

    fn has_all(name: &str, text: &str, want: &[(&str, Class)]) {
        let got = marks(name, text);
        for (w, c) in want {
            assert!(got.iter().any(|(g, gc)| g == w && gc == c), "{name}: no {w:?} as {c:?} in {got:?}");
        }
    }

    #[test]
    fn rust() {
        has_all(
            "main.rs",
            "#[derive(Debug)]\nfn main() {\n    let s: &'static str = \"hi \\\" there\"; // note\n    println!(\"{}\", 'x', '\\n', 42u8, None::<u8>, MAX);\n}",
            &[
                ("#[derive(Debug)]", Meta),
                ("fn", Keyword),
                ("main", Function),
                ("let", Keyword),
                ("'static", Meta),
                ("str", Type),
                ("\"hi \\\" there\"", String),
                ("// note", Comment),
                ("println!", Function),
                ("'x'", String),
                ("'\\n'", String),
                ("42u8", Constant),
                ("None", Type),
                ("MAX", Constant),
            ],
        );
    }

    #[test]
    fn scripts() {
        has_all(
            "deploy.sh",
            "#!/bin/sh\nNAME=x # set it\nif [ \"$#\" -gt 0 ]; then echo \"${NAME}\" $1 'it''s'; fi\n",
            &[
                ("#!/bin/sh", Comment),
                ("NAME", Key),
                ("# set it", Comment),
                ("if", Keyword),
                ("\"$#\"", String),
                ("\"${NAME}\"", String),
                ("$1", Key),
                ("fi", Keyword),
            ],
        );
        has_all(
            "app.py",
            "@cache\ndef f(x):\n    return f'{x}' if x is not None else \"\"\"doc\nstring\"\"\"\n",
            &[
                ("@cache", Meta),
                ("def", Keyword),
                ("f", Function),
                ("'{x}'", String),
                ("None", Constant),
                ("\"\"\"doc\nstring\"\"\"", String),
            ],
        );
    }

    #[test]
    fn apostrophes_are_not_quotes() {
        let got = marks("notes.yaml", "msg: don't panic, it's fine\n");
        assert_eq!(got, vec![("msg".to_string(), Key)]);
    }

    #[test]
    fn json() {
        has_all(
            "package.json",
            "{\"name\": \"narwhal\", \"n\": -1.5e3, \"ok\": true}",
            &[("\"name\"", Key), ("\"narwhal\"", String), ("1.5e3", Constant), ("true", Constant)],
        );
    }

    #[test]
    fn css() {
        has_all(
            "style.css",
            "@media (max-width: 600px) {\n  .menu > a:hover, #main { color: #fff; margin: 0 4px; }\n}",
            &[
                ("@media", Keyword),
                ("max-width", Key),
                ("600px", Constant),
                (".menu", Type),
                ("a", Tag),
                ("#main", Type),
                ("color", Key),
                ("#fff", Constant),
                ("margin", Key),
            ],
        );
    }

    #[test]
    fn html() {
        has_all(
            "bookmarks.html",
            "<!DOCTYPE html>\n<DT><A HREF=\"https://x/\" ADD_DATE=1 download>Tom &amp; Jerry</A>\n<!-- c -->\n<script>let x = '</p>';</script><p>",
            &[
                ("<!DOCTYPE html>", Meta),
                ("DT", Tag),
                ("A", Tag),
                ("HREF", Key),
                ("\"https://x/\"", String),
                ("ADD_DATE", Key),
                ("1", String),
                ("download", Key),
                ("&amp;", Constant),
                ("<!-- c -->", Comment),
                ("script", Tag),
                ("let", Keyword),
                ("'</p>'", String),
                ("p", Tag),
            ],
        );
    }

    #[test]
    fn config_files() {
        has_all(
            "Cargo.toml",
            "# deps\n[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\nlto = true\n",
            &[
                ("# deps", Comment),
                ("[dependencies]", Heading),
                ("serde", Key),
                ("\"1\"", String),
                ("lto", Key),
                ("true", Constant),
            ],
        );
        has_all(
            "ci.yaml",
            "# c\nname: ci\non:\n  push:\n    branches: [dev, main] # x\nsteps:\n  - run: |\n      echo a: b\n  - uses: x@v1\nnext: 1\n",
            &[
                ("# c", Comment),
                ("name", Key),
                ("on", Key),
                ("push", Key),
                ("branches", Key),
                ("# x", Comment),
                ("run", Key),
                ("echo a: b", String),
                ("uses", Key),
                ("next", Key),
                ("1", Constant),
            ],
        );
        // In a block scalar, "echo a" isn't a key.
        assert!(!marks("ci.yaml", "run: |\n  echo a: b\n").contains(&("echo a".to_string(), Key)));
    }

    #[test]
    fn markdown_and_diffs() {
        has_all(
            "README.md",
            "# Title\n\n- see `cargo run` and [docs](https://x)\n\n```rust\nfn x() {}\n```\n",
            &[
                ("# Title", Heading),
                ("-", Keyword),
                ("`cargo run`", String),
                ("https://x", Key),
                ("```rust", Meta),
                ("fn", Keyword),
                ("x", Function),
                ("```", Meta),
            ],
        );
        has_all(
            "fix.patch",
            "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n same\n",
            &[("--- a/x", Meta), ("@@ -1 +1 @@", Function), ("-old", Deleted), ("+new", Inserted)],
        );
    }

    #[test]
    fn html_is_escaped() {
        assert_eq!(
            to_html(lang_for("a.json").unwrap(), "{\"a\": \"<b>&\"}"),
            "{<span class=\"hl-a\">\"a\"</span>: <span class=\"hl-s\">\"&lt;b&gt;&amp;\"</span>}"
        );
    }

    /// The HTML without its spans, unescaped: the text again.
    fn plain(html: &str) -> std::string::String {
        let mut out = std::string::String::new();
        let mut rest = html;
        while let Some(i) = rest.find('<') {
            out.push_str(&rest[..i]);
            rest = &rest[rest[i..].find('>').unwrap() + i + 1..];
        }
        out.push_str(rest);
        out.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
    }

    #[test]
    fn odd_and_cut_off_input_survives() {
        let names = [
            "a.rs", "a.c", "a.go", "a.js", "a.py", "a.rb", "a.sh", "Dockerfile", "a.sql", "a.lua", "a.css", "a.scss",
            "a.json", "a.tex", "a.r", "a.tf", "a.toml", "a.yaml", "a.html", "a.md", "a.diff",
        ];
        let inputs = [
            "\"unterminated",
            "/* open",
            "<a href=\"x",
            "<script>let a = \"",
            "é'ü\"ß` ${x \\",
            "'\\",
            "```rust\nfn é",
            "key: |\n  é",
            "<!--",
            "&#x",
            "$",
            "@",
            "\\",
            "#",
            "- ",
            "1.",
            "'a",
            "café's 'ü' \"ü\" #ü .ü $ü",
        ];
        for name in names {
            for input in inputs {
                let html = to_html(lang_for(name).unwrap(), input);
                assert_eq!(plain(&html), input, "{name}: {input:?} -> {html:?}");
            }
        }
    }

    #[test]
    fn languages() {
        assert!(matches!(lang_for("Dockerfile"), Some(Lang::Code(s)) if std::ptr::eq(s, &DOCKER)));
        assert!(matches!(lang_for("App.TSX"), Some(Lang::Code(s)) if std::ptr::eq(s, &JS)));
        assert!(matches!(lang_for(".env.local"), Some(Lang::Config)));
        assert!(matches!(lang_for("notes.md"), Some(Lang::Markdown)));
        assert!(lang_for("notes.txt").is_none());
        assert!(lang_for("README").is_none());
        assert!(matches!(lang_for_fence("Python"), Some(Lang::Code(s)) if std::ptr::eq(s, &PYTHON)));
    }
}
