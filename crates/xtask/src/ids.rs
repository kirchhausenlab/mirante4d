pub(crate) fn stable_id_from_name(name: &str) -> String {
    let id = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    if id.is_empty() {
        "sample".to_owned()
    } else {
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_id_from_name_is_path_safe_and_stable() {
        assert_eq!(stable_id_from_name("Synthetic source"), "synthetic-source");
        assert_eq!(stable_id_from_name("  !!!  "), "sample");
        assert_eq!(stable_id_from_name("T5-QUAL-001-A-02"), "t5-qual-001-a-02");
    }
}
