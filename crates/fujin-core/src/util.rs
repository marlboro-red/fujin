/// Truncate a string to at most `max_chars` characters (not bytes),
/// appending "..." if truncated. Safe for all UTF-8 content.
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_short_string() {
        assert_eq!(truncate_chars("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact_length() {
        assert_eq!(truncate_chars("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long_string() {
        assert_eq!(truncate_chars("hello world", 5), "hello...");
    }

    #[test]
    fn test_truncate_empty() {
        assert_eq!(truncate_chars("", 5), "");
    }
}
