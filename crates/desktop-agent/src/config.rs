//! Settings file (`~/.config/meshwav/config.toml`): flat `key = value` lines, a small subset of TOML.
//! Command-line flags always win over the file. A key that is not known, a malformed line or a
//! bad value is an error at start-up, so a typo can never silently leave a setting at its default.
use std::collections::HashMap;

pub const KEYS: &[&str] = &["bind", "kbps", "frame_ms", "codec", "source", "no_redirect", "log_level"];

#[derive(Debug, Default, PartialEq)]
pub struct Config(HashMap<String, String>);

impl Config {
    pub fn parse(text: &str) -> Result<Config, String> {
        let mut m = HashMap::new();
        for (n, raw) in text.lines().enumerate() {
            let n = n + 1;
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let (k, v) = line.split_once('=').ok_or_else(|| format!("line {n}: expected `key = value`"))?;
            let k = k.trim();
            if !KEYS.contains(&k) {
                return Err(format!("line {n}: unknown setting `{k}` (known: {})", KEYS.join(", ")));
            }
            let v = v.trim();
            let v = match (v.strip_prefix('"'), v.strip_suffix('"')) {
                (Some(_), Some(_)) if v.len() >= 2 => v[1..v.len() - 1].to_string(),
                (None, None) => v.to_string(),
                _ => return Err(format!("line {n}: unbalanced quotes")),
            };
            if m.insert(k.to_string(), v).is_some() {
                return Err(format!("line {n}: `{k}` is set twice"));
            }
        }
        Ok(Config(m))
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    pub fn flag(&self, key: &str) -> Result<Option<bool>, String> {
        match self.get(key) {
            None => Ok(None),
            Some("true") => Ok(Some(true)),
            Some("false") => Ok(Some(false)),
            Some(v) => Err(format!("`{key}` must be true or false, not `{v}`")),
        }
    }
}

/// Drop a `#` comment, but not a `#` inside double quotes.
fn strip_comment(s: &str) -> &str {
    let mut quoted = false;
    for (i, c) in s.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => return &s[..i],
            _ => {}
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_values_comments_and_quotes() {
        let c = Config::parse("# top\nkbps = 192\nsource = \"a#b.monitor\"  # why\n\nno_redirect = true\n").unwrap();
        assert_eq!(c.get("kbps"), Some("192"));
        assert_eq!(c.get("source"), Some("a#b.monitor"));
        assert_eq!(c.flag("no_redirect"), Ok(Some(true)));
        assert_eq!(c.get("bind"), None);
        assert_eq!(c.flag("bind"), Ok(None));
    }

    #[test]
    fn mistakes_are_errors_with_the_line_number() {
        assert!(Config::parse("kbps 192").unwrap_err().starts_with("line 1"));
        assert!(Config::parse("\nkbbs = 1").unwrap_err().contains("line 2: unknown setting `kbbs`"));
        assert!(Config::parse("kbps = 1\nkbps = 2").unwrap_err().contains("set twice"));
        assert!(Config::parse("source = \"x").unwrap_err().contains("quotes"));
        assert!(Config::parse("no_redirect = yes").unwrap().flag("no_redirect").is_err());
    }

    #[test]
    fn empty_file_is_fine() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        assert_eq!(Config::parse("# nothing\n").unwrap(), Config::default());
    }
}
