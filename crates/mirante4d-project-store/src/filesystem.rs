//! Fail-closed Linux filesystem qualification for writable project stores.

#![cfg(target_os = "linux")]

use std::{
    fs::File,
    io::{Read, Take},
    os::fd::AsFd,
};

use rustix::fs::{AtFlags, FsWord, StatxFlags, fstatfs, statx};

const EXT4_SUPER_MAGIC: FsWord = 0x0000_ef53;
const MOUNTINFO_BYTES_MAX: u64 = 1_048_576;
#[cfg(test)]
pub(crate) const TEST_REAL_POLICY_ENV: &str = "MIRANTE4D_PROJECT_STORE_TEST_REAL_FILESYSTEM_POLICY";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WritableFilesystemQualification {
    mount_id: u64,
}

#[cfg(test)]
impl WritableFilesystemQualification {
    pub(crate) const fn different_for_test(self) -> Self {
        Self {
            mount_id: self.mount_id.wrapping_add(1),
        }
    }
}

pub(crate) fn writable_filesystem_qualification(
    descriptor: impl AsFd,
) -> Option<WritableFilesystemQualification> {
    #[cfg(test)]
    if std::env::var_os(TEST_REAL_POLICY_ENV).is_none() {
        return Some(WritableFilesystemQualification { mount_id: u64::MAX });
    }

    real_writable_filesystem_qualification(descriptor)
}

fn real_writable_filesystem_qualification(
    descriptor: impl AsFd,
) -> Option<WritableFilesystemQualification> {
    let facts = statx(
        descriptor.as_fd(),
        "",
        AtFlags::EMPTY_PATH,
        StatxFlags::MNT_ID,
    )
    .ok()?;
    if facts.stx_mask & StatxFlags::MNT_ID.bits() == 0 {
        return None;
    }
    if fstatfs(descriptor.as_fd()).ok()?.f_type != EXT4_SUPER_MAGIC {
        return None;
    }
    let mountinfo = read_bounded_mountinfo()?;
    if !mountinfo_qualifies(&mountinfo, facts.stx_mnt_id) {
        return None;
    }
    Some(WritableFilesystemQualification {
        mount_id: facts.stx_mnt_id,
    })
}

fn read_bounded_mountinfo() -> Option<String> {
    let file = File::open("/proc/self/mountinfo").ok()?;
    let mut reader: Take<File> = file.take(MOUNTINFO_BYTES_MAX + 1);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).ok()?;
    if u64::try_from(bytes.len()).ok()? > MOUNTINFO_BYTES_MAX {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn mountinfo_qualifies(mountinfo: &str, expected_mount_id: u64) -> bool {
    let mut selected = None;
    for line in mountinfo.lines() {
        let Some(entry) = MountInfoEntry::parse(line) else {
            return false;
        };
        if entry.mount_id != expected_mount_id {
            continue;
        }
        if selected.is_some() {
            return false;
        }
        selected = Some(
            entry.filesystem_type == "ext4"
                && exact_options(entry.vfs_options, &["relatime", "rw"])
                && exact_options(entry.super_options, &["rw"]),
        );
    }
    selected == Some(true)
}

struct MountInfoEntry<'a> {
    mount_id: u64,
    vfs_options: &'a str,
    filesystem_type: &'a str,
    super_options: &'a str,
}

impl<'a> MountInfoEntry<'a> {
    fn parse(line: &'a str) -> Option<Self> {
        let (mount, filesystem) = line.split_once(" - ")?;
        let mount = mount.split_ascii_whitespace().collect::<Vec<_>>();
        let filesystem = filesystem.split_ascii_whitespace().collect::<Vec<_>>();
        if mount.len() < 6 || filesystem.len() != 3 {
            return None;
        }
        Some(Self {
            mount_id: mount[0].parse().ok()?,
            vfs_options: mount[5],
            filesystem_type: filesystem[0],
            super_options: filesystem[2],
        })
    }
}

fn exact_options(actual: &str, expected: &[&str]) -> bool {
    let mut actual = actual.split(',').collect::<Vec<_>>();
    if actual.iter().any(|option| option.is_empty()) {
        return false;
    }
    let option_count = actual.len();
    actual.sort_unstable();
    actual.dedup();
    actual.len() == option_count && actual == expected
}

#[cfg(test)]
mod tests {
    use std::{env, process::Command};

    use super::*;
    use crate::{
        ProjectOpenMode,
        lease::ProjectStoreLeases,
        local::{LocalPublicationError, LocalStoreRoot, ensure_writable_destination},
    };

    const REAL_POLICY_TEST: &str =
        "filesystem::tests::real_policy_rejects_dev_shm_and_downgrades_writes";

    #[test]
    fn parser_accepts_only_the_exact_ext4_mount_tuple() {
        let accepted = "32 2 259:5 / / rw,relatime shared:1 - ext4 /dev/vda rw\n";
        assert!(mountinfo_qualifies(accepted, 32));
        assert!(!mountinfo_qualifies(accepted, 31));

        for rejected in [
            "32 2 259:5 / / rw shared:1 - ext4 /dev/vda rw\n",
            "32 2 259:5 / / rw,relatime,nodev shared:1 - ext4 /dev/vda rw\n",
            "32 2 259:5 / / rw,relatime shared:1 - xfs /dev/vda rw\n",
            "32 2 259:5 / / rw,relatime shared:1 - ext4 /dev/vda rw,errors=remount-ro\n",
            "32 2 259:5 / / rw,rw,relatime shared:1 - ext4 /dev/vda rw\n",
            "malformed\n",
        ] {
            assert!(!mountinfo_qualifies(rejected, 32), "accepted {rejected:?}");
        }

        let duplicate = format!("{accepted}{accepted}");
        assert!(!mountinfo_qualifies(&duplicate, 32));
    }

    #[test]
    fn real_policy_rejects_dev_shm_and_downgrades_writes() {
        if env::var_os(TEST_REAL_POLICY_ENV).is_none() {
            let status = Command::new(env::current_exe().unwrap())
                .arg(REAL_POLICY_TEST)
                .arg("--exact")
                .arg("--nocapture")
                .env(TEST_REAL_POLICY_ENV, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }

        let root = LocalStoreRoot::open(std::path::Path::new("/dev/shm")).unwrap();
        assert!(writable_filesystem_qualification(root.descriptor()).is_none());

        let read_only = ProjectStoreLeases::acquire(&root, ProjectOpenMode::ReadOnly).unwrap();
        assert_eq!(read_only.effective_mode(), ProjectOpenMode::ReadOnly);
        drop(read_only);

        let preferred =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        assert_eq!(preferred.effective_mode(), ProjectOpenMode::ReadOnly);
        assert!(!preferred.has_writer());
        assert!(!preferred.writable_filesystem_qualified());

        let destination = crate::ProjectStorePath::new(format!(
            "/dev/shm/mirante4d-unqualified-{}.m4dproj",
            std::process::id()
        ))
        .unwrap();
        assert!(matches!(
            ensure_writable_destination(&destination),
            Err(LocalPublicationError::AtomicPublishUnsupported)
        ));
        assert!(!destination.as_path().exists());
    }
}
