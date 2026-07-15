use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    for variable in [
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTFLAGS",
        "RUSTC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let repository = manifest
        .parent()
        .and_then(Path::parent)
        .expect("xtask is located below the workspace root");
    register_repository_inputs(repository);

    let head = git_text(repository, &["rev-parse", "--verify", "HEAD"])
        .unwrap_or_else(|| "unavailable".to_owned());
    let dirty = git_output(
        repository,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ],
    )
    .is_none_or(|output| !output.is_empty());
    let compiler = compiler_description();
    let custom_rustflags = environment_value_is_nonempty("CARGO_ENCODED_RUSTFLAGS")
        || environment_value_is_nonempty("RUSTFLAGS");
    let rustc_wrapper = environment_value_is_nonempty("RUSTC_WRAPPER")
        || environment_value_is_nonempty("RUSTC_WORKSPACE_WRAPPER");

    emit("MIRANTE4D_XTASK_BUILD_GIT_HEAD", &head);
    emit(
        "MIRANTE4D_XTASK_BUILD_GIT_DIRTY",
        if dirty { "true" } else { "false" },
    );
    emit(
        "MIRANTE4D_XTASK_BUILD_CARGO_PROFILE",
        &env::var("PROFILE").unwrap_or_else(|_| "unavailable".to_owned()),
    );
    emit(
        "MIRANTE4D_XTASK_BUILD_OPT_LEVEL",
        &env::var("OPT_LEVEL").unwrap_or_else(|_| "unavailable".to_owned()),
    );
    emit(
        "MIRANTE4D_XTASK_BUILD_DEBUG",
        &env::var("DEBUG").unwrap_or_else(|_| "unavailable".to_owned()),
    );
    emit("MIRANTE4D_XTASK_BUILD_COMPILER", &compiler);
    emit(
        "MIRANTE4D_XTASK_BUILD_CUSTOM_RUSTFLAGS",
        if custom_rustflags { "true" } else { "false" },
    );
    emit(
        "MIRANTE4D_XTASK_BUILD_RUSTC_WRAPPER",
        if rustc_wrapper { "true" } else { "false" },
    );
}

fn register_repository_inputs(repository: &Path) {
    if let Some(paths) = git_text(repository, &["ls-files"]) {
        for relative in paths.lines().filter(|path| !path.is_empty()) {
            println!(
                "cargo:rerun-if-changed={}",
                repository.join(relative).display()
            );
        }
    } else {
        println!("cargo:rerun-if-changed={}", repository.display());
    }

    for arguments in [
        &["rev-parse", "--git-path", "HEAD"][..],
        &["rev-parse", "--git-path", "index"][..],
        &["rev-parse", "--git-path", "packed-refs"][..],
    ] {
        register_git_path(repository, arguments);
    }
    if let Some(reference) = git_text(repository, &["symbolic-ref", "-q", "HEAD"]) {
        register_git_path(repository, &["rev-parse", "--git-path", reference.as_str()]);
    }
}

fn register_git_path(repository: &Path, arguments: &[&str]) {
    let Some(path) = git_text(repository, arguments) else {
        return;
    };
    let path = PathBuf::from(path);
    let path = if path.is_absolute() {
        path
    } else {
        repository.join(path)
    };
    println!("cargo:rerun-if-changed={}", path.display());
}

fn compiler_description() -> String {
    let compiler = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    Command::new(compiler)
        .arg("--version")
        .arg("--verbose")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| sanitize(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn environment_value_is_nonempty(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn git_text(repository: &Path, arguments: &[&str]) -> Option<String> {
    let output = git_output(repository, arguments)?;
    let value = String::from_utf8(output).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn git_output(repository: &Path, arguments: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn sanitize(value: &str) -> String {
    value
        .trim()
        .replace('\r', "")
        .replace('\n', "\\n")
        .replace('\0', "")
}

fn emit(name: &str, value: &str) {
    println!("cargo:rustc-env={name}={}", sanitize(value));
}
