use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

const PSEUDO_CFG: &str = "#[cfg(all(debug_assertions, any(feature = \"dev-pseudo\", test)))]";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Schema {
    messages: BTreeMap<String, Message>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    context: String,
    #[serde(default)]
    args: Vec<Argument>,
    plural: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Argument {
    name: String,
    #[serde(rename = "type")]
    ty: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Entry {
    Text(String),
    Plural(Branches),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Branches {
    one: String,
    other: String,
}

type Catalogue = BTreeMap<String, Entry>;

#[derive(Debug, PartialEq)]
enum Token {
    Literal(String),
    Argument(String),
}

fn identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|c| c.is_ascii_lowercase())
        && bytes.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        && !name.ends_with('_')
        && !name.contains("__")
        && ![
            "as",
            "async",
            "await",
            "break",
            "const",
            "continue",
            "crate",
            "dyn",
            "else",
            "enum",
            "extern",
            "false",
            "fn",
            "for",
            "if",
            "impl",
            "in",
            "let",
            "loop",
            "match",
            "mod",
            "move",
            "mut",
            "pub",
            "ref",
            "return",
            "self",
            "static",
            "struct",
            "super",
            "trait",
            "true",
            "type",
            "unsafe",
            "use",
            "where",
            "while",
            "abstract",
            "become",
            "box",
            "do",
            "final",
            "gen",
            "macro",
            "override",
            "priv",
            "try",
            "typeof",
            "unsized",
            "virtual",
            "yield",
            "new",
            "text",
            "language",
            "pseudo",
            "spanish_entry",
        ]
        .contains(&name)
}

fn tokens(text: &str) -> Result<Vec<Token>, String> {
    let mut result = Vec::new();
    let mut literal = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                literal.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                literal.push('}');
            }
            '{' => {
                if !literal.is_empty() {
                    result.push(Token::Literal(std::mem::take(&mut literal)));
                }
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => return Err("unclosed placeholder".into()),
                    }
                }
                if !identifier(&name) {
                    return Err(format!("invalid placeholder {name:?}"));
                }
                result.push(Token::Argument(name));
            }
            '}' => return Err("unmatched closing brace".into()),
            c => literal.push(c),
        }
    }
    if !literal.is_empty() {
        result.push(Token::Literal(literal));
    }
    Ok(result)
}

fn validate(schema: &Schema, catalogue: &Catalogue, locale: &str) -> Result<(), String> {
    let expected: BTreeSet<_> = schema.messages.keys().collect();
    let actual: BTreeSet<_> = catalogue.keys().collect();
    if expected != actual {
        return Err(format!(
            "{locale}: missing {:?}; extra {:?}",
            expected.difference(&actual).collect::<Vec<_>>(),
            actual.difference(&expected).collect::<Vec<_>>()
        ));
    }
    for (key, message) in &schema.messages {
        let texts = match (&catalogue[key], &message.plural) {
            (Entry::Text(text), None) => vec![text],
            (Entry::Plural(branches), Some(_)) => vec![&branches.one, &branches.other],
            _ => {
                return Err(format!(
                    "{locale}/{key}: expected schema's text/plural shape"
                ));
            }
        };
        let expected: BTreeSet<_> = message.args.iter().map(|arg| arg.name.as_str()).collect();
        for text in texts {
            if text.is_empty() || text.len() > 16_384 {
                return Err(format!("{locale}/{key}: empty or oversized text"));
            }
            let parsed = tokens(text).map_err(|e| format!("{locale}/{key}: {e}"))?;
            let actual: BTreeSet<_> = parsed
                .iter()
                .filter_map(|token| match token {
                    Token::Argument(name) => Some(name.as_str()),
                    Token::Literal(_) => None,
                })
                .collect();
            if actual != expected {
                return Err(format!(
                    "{locale}/{key}: placeholders {actual:?}, expected {expected:?}"
                ));
            }
        }
    }
    Ok(())
}

fn variant(key: &str) -> String {
    key.split('_')
        .map(|part| {
            let mut chars = part.chars();
            let first = chars
                .next()
                .expect("validated identifier")
                .to_ascii_uppercase();
            format!("{first}{}", chars.as_str())
        })
        .collect()
}

fn pseudo_literal(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'a' => 'á',
            'e' => 'é',
            'i' => 'í',
            'o' => 'ó',
            'u' => 'ú',
            'A' => 'Á',
            'E' => 'É',
            'I' => 'Í',
            'O' => 'Ó',
            'U' => 'Ú',
            _ => c,
        })
        .collect()
}

fn template(text: &str, pseudo: bool, formatted: bool) -> String {
    let parsed = tokens(text).expect("validated template");
    let mut out = String::new();
    let literal_len: usize = parsed
        .iter()
        .map(|t| match t {
            Token::Literal(s) => s.chars().count(),
            _ => 0,
        })
        .sum();
    if pseudo {
        out.push('⟦');
    }
    for token in parsed {
        match token {
            Token::Literal(s) => {
                let s = if pseudo { pseudo_literal(&s) } else { s };
                if formatted {
                    out.push_str(&s.replace('{', "{{").replace('}', "}}"));
                } else {
                    out.push_str(&s);
                }
            }
            Token::Argument(name) => {
                write!(out, "{{{name}}}").unwrap();
            }
        }
    }
    if pseudo {
        // Delimiters count toward the target; tiny labels necessarily round.
        let expansion = (literal_len * 38 + 50) / 100;
        out.push_str(&"~".repeat(expansion.saturating_sub(2)));
        out.push('⟧');
    }
    out
}

fn expression(entry: &Entry, message: &Message, pseudo: bool) -> String {
    let render = |text: &str| {
        let text = template(text, pseudo, !message.args.is_empty());
        if message.args.is_empty() {
            format!("{text:?}")
        } else {
            // Only validated schema identifiers enter syntax. Text is always a Rust literal.
            let args = message
                .args
                .iter()
                .map(|arg| format!(", {} = {}", arg.name, arg.name))
                .collect::<String>();
            format!("format!({text:?}{args})")
        }
    };
    match entry {
        Entry::Text(text) => render(text),
        Entry::Plural(branches) => format!(
            "if {} == 1 {{ {} }} else {{ {} }}",
            message.plural.as_ref().unwrap(),
            render(&branches.one),
            render(&branches.other)
        ),
    }
}

pub fn generate(schema: &str, en: &str, es: &str) -> Result<String, String> {
    let schema: Schema = toml::from_str(schema).map_err(|e| format!("schema: {e}"))?;
    if schema.messages.is_empty() {
        return Err("empty schema".into());
    }
    let mut variants = BTreeSet::new();
    for (key, message) in &schema.messages {
        if !identifier(key) || message.context.trim().is_empty() {
            return Err(format!("invalid key/context {key:?}"));
        }
        if message.args.is_empty() && !variants.insert(variant(key)) {
            return Err(format!("colliding enum variant {key}"));
        }
        let mut args = BTreeSet::new();
        for arg in &message.args {
            if !identifier(&arg.name)
                || !matches!(arg.ty.as_str(), "str" | "u64")
                || !args.insert(&arg.name)
            {
                return Err(format!("{key}: invalid/duplicate argument"));
            }
        }
        if let Some(plural) = &message.plural
            && !message
                .args
                .iter()
                .any(|arg| arg.name == *plural && arg.ty == "u64")
        {
            return Err(format!(
                "{key}: plural selector must be a declared u64 argument"
            ));
        }
    }
    let en: Catalogue = toml::from_str(en).map_err(|e| format!("en: {e}"))?;
    let es: Catalogue = toml::from_str(es).map_err(|e| format!("es: {e}"))?;
    validate(&schema, &en, "en")?;
    validate(&schema, &es, "es")?;
    let mut out = String::from(
        "// Generated from validated catalogues. Do not edit.\n#[derive(Clone, Copy, Debug, Eq, PartialEq)]\npub enum Text {\n",
    );
    for (key, msg) in &schema.messages {
        if msg.args.is_empty() {
            writeln!(out, "{},", variant(key)).unwrap();
        }
    }
    out.push_str("}\nimpl Text { pub const ALL: &'static [Self] = &[\n");
    for (key, msg) in &schema.messages {
        if msg.args.is_empty() {
            writeln!(out, "Self::{},", variant(key)).unwrap();
        }
    }
    out.push_str("]; }\nimpl Translator {\npub fn text(&self, key: Text) -> &'static str {\n");
    writeln!(out, "{PSEUDO_CFG}\nif self.pseudo {{ return match key {{").unwrap();
    for (key, msg) in &schema.messages {
        if msg.args.is_empty() {
            writeln!(
                out,
                "Text::{} => {},",
                variant(key),
                expression(&en[key], msg, true)
            )
            .unwrap();
        }
    }
    out.push_str("}; }\nlet english = match key {\n");
    for (key, msg) in &schema.messages {
        if msg.args.is_empty() {
            writeln!(
                out,
                "Text::{} => {},",
                variant(key),
                expression(&en[key], msg, false)
            )
            .unwrap();
        }
    }
    out.push_str(
        "};\nif self.language == Language::En { return english; }\nlet spanish = match key {\n",
    );
    for (key, msg) in &schema.messages {
        if msg.args.is_empty() {
            writeln!(
                out,
                "Text::{} => Some({}),",
                variant(key),
                expression(&es[key], msg, false)
            )
            .unwrap();
        }
    }
    out.push_str("};\nself.spanish_entry(spanish).unwrap_or(english)\n}\n");
    for (key, msg) in &schema.messages {
        if msg.args.is_empty() {
            continue;
        }
        let args = msg
            .args
            .iter()
            .map(|arg| {
                format!(
                    ", {}: {}",
                    arg.name,
                    if arg.ty == "str" { "&str" } else { "u64" }
                )
            })
            .collect::<String>();
        writeln!(out, "pub fn {key}(&self{args}) -> String {{").unwrap();
        writeln!(
            out,
            "{PSEUDO_CFG}\nif self.pseudo {{ return {}; }}",
            expression(&en[key], msg, true)
        )
        .unwrap();
        // The fallback chooses a branch before formatting, so arguments are preserved.
        writeln!(out, "if self.language == Language::Es && self.spanish_entry(Some(())).is_some() {{ {} }} else {{ {} }}\n}}", expression(&es[key], msg, false), expression(&en[key], msg, false)).unwrap();
    }
    out.push_str("}\n");
    Ok(out)
}
