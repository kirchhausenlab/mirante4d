#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolKind {
    #[default]
    Navigate,
    Inspect,
    Crosshair,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_is_the_neutral_default_tool() {
        assert_eq!(ToolKind::default(), ToolKind::Navigate);
    }
}
