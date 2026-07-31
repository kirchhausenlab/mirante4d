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
    canonical_release_environment_reason_codes(variables, target, compiler).is_empty()
}

pub(crate) fn canonical_release_environment_reason_codes(
    variables: &BTreeMap<OsString, OsString>,
    target: &str,
    compiler: &str,
) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    for (name, expected) in CANONICAL_RELEASE_ENVIRONMENT
        .iter()
        .chain(std::iter::once(&CANONICAL_RELEASE_STRIP))
    {
        if variables.get(OsStr::new(name)) != Some(&OsString::from(expected)) {
            reasons.push(match *name {
                "MIRANTE4D_XTASK_QUALIFICATION_BUILD" => "qualification_marker_mismatch",
                "CARGO_PROFILE_RELEASE_OPT_LEVEL" => "release_opt_level_mismatch",
                "CARGO_PROFILE_RELEASE_DEBUG" => "release_debug_mismatch",
                "CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS" => "release_debug_assertions_mismatch",
                "CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS" => "release_overflow_checks_mismatch",
                "CARGO_PROFILE_RELEASE_INCREMENTAL" => "release_incremental_mismatch",
                "CARGO_PROFILE_RELEASE_LTO" => "release_lto_mismatch",
                "CARGO_PROFILE_RELEASE_PANIC" => "release_panic_mismatch",
                "CARGO_PROFILE_RELEASE_CODEGEN_UNITS" => "release_codegen_units_mismatch",
                "CARGO_PROFILE_RELEASE_RPATH" => "release_rpath_mismatch",
                "CARGO_PROFILE_RELEASE_STRIP" => "release_strip_mismatch",
                _ => "release_environment_mismatch",
            });
        }
    }
    if !variables.iter().all(|(name, value)| {
        let name = name.to_string_lossy();
        !name.starts_with("CARGO_PROFILE_RELEASE_")
            || CANONICAL_RELEASE_ENVIRONMENT
                .iter()
                .chain(std::iter::once(&CANONICAL_RELEASE_STRIP))
                .any(|(accepted, _)| name == *accepted)
            || value.is_empty()
    }) {
        reasons.push("unexpected_release_profile_override");
    }
    for (name, reason) in [
        ("CARGO_BUILD_TARGET", "cargo_build_target_override"),
        ("CARGO_INCREMENTAL", "cargo_incremental_override"),
        ("RUSTC_BOOTSTRAP", "rustc_bootstrap_override"),
    ] {
        if variables
            .get(OsStr::new(name))
            .is_some_and(|value| !value.is_empty())
        {
            reasons.push(reason);
        }
    }
    if compiler
        .lines()
        .flat_map(|line| line.split("\\n"))
        .find_map(|line| line.strip_prefix("host: "))
        != Some(target)
    {
        reasons.push("compiler_host_target_mismatch");
    }
    reasons.sort_unstable();
    reasons.dedup();
    reasons
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
