#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;
use mirante4d_format::{NativeManifest, validate_manifest_quick};

fuzz_target!(|data: &[u8]| {
    if let Ok(manifest) = serde_json::from_slice::<NativeManifest>(data) {
        let _ = validate_manifest_quick(Path::new("/tmp/mirante4d-fuzz-native.m4d"), &manifest);
    }
});
