//! Rules for names users give installs. Shared by every driver, so an install name means the same
//! thing whichever driver made it.

/// Why `name` cannot name an install, if it cannot. It becomes a file name for local installs, and
/// part of a Secret's name and a label value on Kubernetes, so it must be a DNS label short enough
/// to leave room for a prefix.
pub fn install_name_problem(name: &str) -> Option<String> {
    dns_label_problem(name, 40)
}

/// Why `name` cannot be a Kubernetes namespace, if it cannot.
pub fn namespace_problem(name: &str) -> Option<String> {
    dns_label_problem(name, 63)
}

/// Why `name` is not a DNS label of at most `longest` characters, if it is not.
fn dns_label_problem(name: &str, longest: usize) -> Option<String> {
    let edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let inside = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if name.is_empty()
        || name.len() > longest
        || !inside
        || !edge(name.chars().next())
        || !edge(name.chars().last())
    {
        return Some(format!(
            "\"{name}\" must be 1 to {longest} lowercase letters, digits or hyphens, starting \
             and ending with a letter or digit"
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_names() {
        assert_eq!(install_name_problem("blog"), None);
        assert_eq!(install_name_problem("a-1"), None);
        for bad in ["", "-a", "a-", "Blog", "a_b", &"a".repeat(41)] {
            assert!(install_name_problem(bad).is_some(), "{bad:?}");
        }
        assert_eq!(namespace_problem(&"a".repeat(63)), None);
    }
}
