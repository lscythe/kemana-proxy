use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::SystemTime;

use super::BundleStore;
use super::store::version_of_file_name;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneFailure {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PruneReport {
    pub removed_versions: Vec<String>,
    pub failures: Vec<PruneFailure>,
}

struct Candidate {
    version: String,
    modified: SystemTime,
}

/// Removes bundle files so that at most `keep` remain, counting
/// `current_version`, which is never removed whatever its age (a rebuild from
/// an older source points the manifest back at an old file). The rest are
/// kept newest first by modification time, ties going to the greater version.
///
/// Only regular files named exactly `<version>.sqlite.zst` are candidates;
/// symlinks, directories and every other name are left alone. A file that
/// cannot be inspected or removed is reported and skipped, never fatal, since
/// the build that called this has already published.
pub fn prune(store: &BundleStore, current_version: &str, keep: NonZeroUsize) -> PruneReport {
    let mut report = PruneReport::default();
    let mut candidates = list_candidates(store, current_version, &mut report);
    // Oldest first, so the survivors are the tail.
    candidates.sort_by(|a, b| (a.modified, &a.version).cmp(&(b.modified, &b.version)));
    let others_to_keep = keep.get() - 1;
    let removable = candidates.len().saturating_sub(others_to_keep);

    for candidate in &candidates[..removable] {
        let Some(path) = store.bundle_path(&candidate.version) else {
            continue;
        };
        match std::fs::remove_file(&path) {
            Ok(()) => report.removed_versions.push(candidate.version.clone()),
            Err(error) => report.failures.push(PruneFailure {
                path,
                reason: error.to_string(),
            }),
        }
    }
    report
}

fn list_candidates(
    store: &BundleStore,
    current_version: &str,
    report: &mut PruneReport,
) -> Vec<Candidate> {
    let entries = match std::fs::read_dir(store.dir()) {
        Ok(entries) => entries,
        Err(error) => {
            report.failures.push(PruneFailure {
                path: store.dir().to_owned(),
                reason: error.to_string(),
            });
            return Vec::new();
        }
    };
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.failures.push(PruneFailure {
                    path: store.dir().to_owned(),
                    reason: error.to_string(),
                });
                continue;
            }
        };
        let file_name = entry.file_name();
        let Some(version) = file_name.to_str().and_then(version_of_file_name) else {
            continue;
        };
        if version == current_version {
            continue;
        }
        // DirEntry metadata does not follow symlinks.
        let inspected = entry.metadata().and_then(|metadata| {
            let modified = metadata.modified()?;
            Ok(metadata.is_file().then_some(modified))
        });
        match inspected {
            Ok(Some(modified)) => candidates.push(Candidate {
                version: version.to_owned(),
                modified,
            }),
            Ok(None) => {}
            Err(error) => report.failures.push(PruneFailure {
                path: entry.path(),
                reason: error.to_string(),
            }),
        }
    }
    candidates
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn a_file_that_cannot_be_removed_is_reported_and_the_rest_are_still_tried() {
        let dir = tempfile::tempdir().unwrap();
        let versions = [
            "1-00000000000000a1",
            "1-00000000000000a2",
            "1-00000000000000a3",
        ];
        for version in versions {
            std::fs::write(dir.path().join(format!("{version}.sqlite.zst")), b"bundle").unwrap();
        }
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let deletion_is_blocked = std::fs::File::create(dir.path().join("probe")).is_err();

        let report = prune(
            &BundleStore::new(dir.path()),
            "1-00000000000000a3",
            NonZeroUsize::new(1).unwrap(),
        );

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        // A superuser ignores directory permissions, so nothing can be provoked.
        if !deletion_is_blocked {
            return;
        }
        assert!(report.removed_versions.is_empty());
        let mut failed: Vec<String> = report
            .failures
            .iter()
            .map(|failure| {
                failure
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        failed.sort();
        assert_eq!(
            failed,
            [
                "1-00000000000000a1.sqlite.zst",
                "1-00000000000000a2.sqlite.zst"
            ]
        );
        for version in versions {
            assert!(dir.path().join(format!("{version}.sqlite.zst")).exists());
        }
    }
}
