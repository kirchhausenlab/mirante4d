#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;
use mirante4d_app::parse_project_session_manifest;

fuzz_target!(|data: &[u8]| {
    if let Ok(encoded) = std::str::from_utf8(data) {
        let _ = parse_project_session_manifest(
            Path::new("/tmp/mirante4d-fuzz-project.m4dproj"),
            encoded,
        );
    }
});
