use super::*;

pub(super) fn prepare_package_root(
    package_root: &Path,
    existing_policy: ExistingPackagePolicy,
) -> Result<(), FormatError> {
    if package_root.exists() {
        match existing_policy {
            ExistingPackagePolicy::Fail => {
                return Err(FormatError::PackageExists(package_root.to_path_buf()));
            }
            ExistingPackagePolicy::Replace => {
                fs::remove_dir_all(package_root).map_err(|source| FormatError::WriteManifest {
                    path: package_root.to_path_buf(),
                    source,
                })?;
            }
        }
    }
    fs::create_dir_all(package_root).map_err(|source| FormatError::WriteManifest {
        path: package_root.to_path_buf(),
        source,
    })
}
