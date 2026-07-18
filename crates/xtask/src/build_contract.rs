use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
};

pub(crate) const QUALIFICATION_BUILD_MARKER: &str = "canonical-release-1";

pub(crate) const CANONICAL_RELEASE_ENVIRONMENT: [(&str, &str); 10] = [
    (
        "MIRANTE4D_XTASK_QUALIFICATION_BUILD",
        QUALIFICATION_BUILD_MARKER,
    ),
    ("CARGO_PROFILE_RELEASE_OPT_LEVEL", "3"),
    ("CARGO_PROFILE_RELEASE_DEBUG", "false"),
    ("CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS", "false"),
    ("CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS", "false"),
    ("CARGO_PROFILE_RELEASE_INCREMENTAL", "false"),
    ("CARGO_PROFILE_RELEASE_LTO", "false"),
    ("CARGO_PROFILE_RELEASE_PANIC", "unwind"),
    ("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "16"),
    ("CARGO_PROFILE_RELEASE_RPATH", "false"),
];

pub(crate) const CANONICAL_RELEASE_STRIP: (&str, &str) = ("CARGO_PROFILE_RELEASE_STRIP", "none");

pub(crate) fn canonical_release_environment_matches(
    variables: &BTreeMap<OsString, OsString>,
    target: &str,
    compiler: &str,
) -> bool {
    CANONICAL_RELEASE_ENVIRONMENT
        .iter()
        .chain(std::iter::once(&CANONICAL_RELEASE_STRIP))
        .all(|(name, expected)| variables.get(OsStr::new(name)) == Some(&OsString::from(expected)))
        && variables.iter().all(|(name, value)| {
            let name = name.to_string_lossy();
            !name.starts_with("CARGO_PROFILE_RELEASE_")
                || CANONICAL_RELEASE_ENVIRONMENT
                    .iter()
                    .chain(std::iter::once(&CANONICAL_RELEASE_STRIP))
                    .any(|(accepted, _)| name == *accepted)
                || value.is_empty()
        })
        && ["CARGO_BUILD_TARGET", "CARGO_INCREMENTAL", "RUSTC_BOOTSTRAP"]
            .iter()
            .all(|name| {
                variables
                    .get(OsStr::new(name))
                    .is_none_or(|value| value.is_empty())
            })
        && compiler
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            == Some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matching_environment() -> BTreeMap<OsString, OsString> {
        CANONICAL_RELEASE_ENVIRONMENT
            .iter()
            .chain(std::iter::once(&CANONICAL_RELEASE_STRIP))
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }

    #[test]
    fn every_release_dimension_is_mandatory_and_exact() {
        let environment = matching_environment();
        let compiler = "rustc 1.90.0\nhost: x86_64-unknown-linux-gnu\n";
        assert!(canonical_release_environment_matches(
            &environment,
            "x86_64-unknown-linux-gnu",
            compiler,
        ));

        for (name, _) in CANONICAL_RELEASE_ENVIRONMENT
            .iter()
            .chain(std::iter::once(&CANONICAL_RELEASE_STRIP))
        {
            let mut missing = environment.clone();
            missing.remove(OsStr::new(name));
            assert!(
                !canonical_release_environment_matches(
                    &missing,
                    "x86_64-unknown-linux-gnu",
                    compiler,
                ),
                "missing {name} was accepted"
            );

            let mut changed = environment.clone();
            changed.insert(OsString::from(name), OsString::from("nonstandard"));
            assert!(
                !canonical_release_environment_matches(
                    &changed,
                    "x86_64-unknown-linux-gnu",
                    compiler,
                ),
                "changed {name} was accepted"
            );
        }
    }

    #[test]
    fn target_extra_profile_and_global_overrides_fail_closed() {
        let compiler = "rustc 1.90.0\nhost: x86_64-unknown-linux-gnu\n";
        for name in [
            "CARGO_BUILD_TARGET",
            "CARGO_INCREMENTAL",
            "RUSTC_BOOTSTRAP",
            "CARGO_PROFILE_RELEASE_BUILD_OVERRIDE_OPT_LEVEL",
        ] {
            let mut environment = matching_environment();
            environment.insert(OsString::from(name), OsString::from("changed"));
            assert!(!canonical_release_environment_matches(
                &environment,
                "x86_64-unknown-linux-gnu",
                compiler,
            ));
        }
        assert!(!canonical_release_environment_matches(
            &matching_environment(),
            "aarch64-unknown-linux-gnu",
            compiler,
        ));
    }
}
