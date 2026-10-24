//! Minimaler Parser für Valves KeyValues-Textformat (.acf / .vdf).
//! Unterstützt verschachtelte Objekte, Escapes, `//`-Kommentare und unquotierte Tokens.

use std::fmt;

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Obj(Obj),
}

/// Geordnete Schlüssel/Wert-Liste (Valve erlaubt doppelte Schlüssel).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Obj(pub Vec<(String, Value)>);

impl Obj {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v)
    }
    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Value::Str(s) => Some(s),
            Value::Obj(_) => None,
        }
    }
    pub fn obj(&self, key: &str) -> Option<&Obj> {
        match self.get(key)? {
            Value::Obj(o) => Some(o),
            Value::Str(_) => None,
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = &(String, Value)> {
        self.0.iter()
    }
}

#[derive(Debug, PartialEq)]
pub struct VdfError {
    pub offset: usize,
    pub msg: &'static str,
}

impl fmt::Display for VdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VDF-Fehler bei Byte {}: {}", self.offset, self.msg)
    }
}
impl std::error::Error for VdfError {}

#[derive(Debug, PartialEq)]
enum Tok {
    Str(String),
    Open,
    Close,
}

fn lex(text: &str) -> Result<Vec<(usize, Tok)>, VdfError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let b = text.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        match b[i] {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'{' => {
                out.push((i, Tok::Open));
                i += 1;
            }
            b'}' => {
                out.push((i, Tok::Close));
                i += 1;
            }
            b'"' => {
                let start = i;
                i += 1;
                let mut s = Vec::new();
                loop {
                    let Some(&c) = b.get(i) else {
                        return Err(VdfError { offset: start, msg: "String nicht geschlossen" });
                    };
                    match c {
                        b'"' => {
                            i += 1;
                            break;
                        }
                        b'\\' => match b.get(i + 1) {
                            Some(b'\\') => {
                                s.push(b'\\');
                                i += 2;
                            }
                            Some(b'"') => {
                                s.push(b'"');
                                i += 2;
                            }
                            Some(b'n') => {
                                s.push(b'\n');
                                i += 2;
                            }
                            Some(b't') => {
                                s.push(b'\t');
                                i += 2;
                            }
                            _ => {
                                s.push(b'\\');
                                i += 1;
                            }
                        },
                        _ => {
                            s.push(c);
                            i += 1;
                        }
                    }
                }
                out.push((start, Tok::Str(String::from_utf8_lossy(&s).into_owned())));
            }
            _ => {
                // unquotiertes Token, z. B. Bedingungen wie [$WIN32]
                let start = i;
                while i < b.len() && !matches!(b[i], b' ' | b'\t' | b'\r' | b'\n' | b'{' | b'}' | b'"') {
                    i += 1;
                }
                out.push((start, Tok::Str(text[start..i].to_string())));
            }
        }
    }
    Ok(out)
}

fn parse_obj(toks: &[(usize, Tok)], pos: &mut usize, depth: usize, top: bool) -> Result<Obj, VdfError> {
    if depth > MAX_DEPTH {
        return Err(VdfError { offset: toks.get(*pos).map_or(0, |t| t.0), msg: "zu tief verschachtelt" });
    }
    let mut obj = Obj::default();
    loop {
        let Some((off, tok)) = toks.get(*pos) else {
            return if top { Ok(obj) } else { Err(VdfError { offset: toks.last().map_or(0, |t| t.0), msg: "Objekt nicht geschlossen" }) };
        };
        match tok {
            Tok::Close => {
                if top {
                    return Err(VdfError { offset: *off, msg: "unerwartetes '}'" });
                }
                *pos += 1;
                return Ok(obj);
            }
            Tok::Open => return Err(VdfError { offset: *off, msg: "'{' ohne Schlüssel" }),
            Tok::Str(key) => {
                *pos += 1;
                match toks.get(*pos) {
                    Some((_, Tok::Str(v))) => {
                        obj.0.push((key.clone(), Value::Str(v.clone())));
                        *pos += 1;
                    }
                    Some((_, Tok::Open)) => {
                        *pos += 1;
                        let child = parse_obj(toks, pos, depth + 1, false)?;
                        obj.0.push((key.clone(), Value::Obj(child)));
                    }
                    _ => return Err(VdfError { offset: *off, msg: "Wert fehlt" }),
                }
            }
        }
    }
}

pub fn parse(text: &str) -> Result<Obj, VdfError> {
    let toks = lex(text)?;
    let mut pos = 0;
    parse_obj(&toks, &mut pos, 0, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_and_case_insensitive() {
        let o = parse("\"AppState\"\n{\n\t\"appid\"\t\"620\"\n\t\"Name\" \"Portal 2\"\n\t\"UserConfig\" { \"language\" \"english\" }\n}").unwrap();
        let s = o.obj("appstate").unwrap();
        assert_eq!(s.str("APPID"), Some("620"));
        assert_eq!(s.str("name"), Some("Portal 2"));
        assert_eq!(s.obj("userconfig").unwrap().str("language"), Some("english"));
    }

    #[test]
    fn escapes_and_comments() {
        let o = parse("// Kommentar\n\"k\" \"D:\\\\Steam Library\\\\x\"\n\"q\" \"sag \\\"hi\\\"\"").unwrap();
        assert_eq!(o.str("k"), Some("D:\\Steam Library\\x"));
        assert_eq!(o.str("q"), Some("sag \"hi\""));
    }

    #[test]
    fn bom_unquoted_and_duplicates() {
        let o = parse("\u{feff}\"a\" \"1\"\n\"a\" \"2\"\nkey [$WIN32]\n").unwrap();
        assert_eq!(o.str("a"), Some("1")); // erster Treffer gewinnt
        assert_eq!(o.0.iter().filter(|(k, _)| k == "a").count(), 2);
        assert_eq!(o.str("key"), Some("[$WIN32]"));
    }

    #[test]
    fn errors_do_not_panic() {
        assert!(parse("\"a\" {").is_err());
        assert!(parse("}").is_err());
        assert!(parse("\"unclosed").is_err());
        assert!(parse("\"a\"").is_err());
        let deep = "\"a\" {".repeat(100);
        assert!(parse(&deep).is_err());
        assert!(parse("").unwrap().0.is_empty());
    }
}
