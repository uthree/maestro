//! Quoting for the POSIX shell `ssh` runs a remote command in.

/// Quotes a word so the shell passes it through unchanged. Plain words are left alone, so a
/// logged command line stays readable.
pub fn quote(word: &str) -> String {
    if !word.is_empty()
        && word.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-=:,@+%".contains(&b))
    {
        return word.into();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_only_what_needs_quoting() {
        assert_eq!(quote("maestro-daemon"), "maestro-daemon");
        assert_eq!(quote("/opt/maestro/bin/maestro-daemon"), "/opt/maestro/bin/maestro-daemon");
        assert_eq!(quote("A=1"), "A=1");
        assert_eq!(quote("my daemon"), "'my daemon'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote("~/bin"), "'~/bin'");
        assert_eq!(quote("$HOME"), "'$HOME'");
        assert_eq!(quote(""), "''");
    }
}
