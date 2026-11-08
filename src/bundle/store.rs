use std::path::{Path, PathBuf};

const MANIFEST_FILE: &str = "manifest.pb";
const BUNDLE_EXTENSION: &str = ".sqlite.zst";

/// The on-disk layout of a bundle directory: one `<version>.sqlite.zst` per
/// built source, and `manifest.pb` describing the latest one.
#[derive(Debug, Clone)]
pub struct BundleStore {
    dir: PathBuf,
}

impl BundleStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.dir.join(MANIFEST_FILE)
    }

    /// `None` when `version` is not shaped like a bundle version, so a request
    /// path can never reach a file outside the store.
    pub fn bundle_path(&self, version: &str) -> Option<PathBuf> {
        is_bundle_version(version).then(|| self.dir.join(format!("{version}{BUNDLE_EXTENSION}")))
    }
}

fn is_bundle_version(version: &str) -> bool {
    let Some((schema_version, digest)) = version.split_once('-') else {
        return false;
    };
    !schema_version.is_empty()
        && schema_version.bytes().all(|byte| byte.is_ascii_digit())
        && digest.len() == super::VERSION_DIGEST_LENGTH
        && digest
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_lives_at_the_root_of_the_store() {
        assert_eq!(
            BundleStore::new("/srv/bundles").manifest_path(),
            Path::new("/srv/bundles/manifest.pb")
        );
    }

    #[test]
    fn a_well_formed_version_maps_to_its_compressed_database() {
        let store = BundleStore::new("/srv/bundles");

        assert_eq!(
            store.bundle_path("1-0123456789abcdef"),
            Some(PathBuf::from("/srv/bundles/1-0123456789abcdef.sqlite.zst"))
        );
    }

    #[test]
    fn anything_else_is_not_a_bundle_version() {
        let store = BundleStore::new("/srv/bundles");

        for version in [
            "",
            "manifest",
            "1-0123456789ABCDEF",
            "1-0123456789abcde",
            "1-0123456789abcdef0",
            "-0123456789abcdef",
            "x-0123456789abcdef",
            "../1-0123456789abcdef",
            "1-0123456789abcdef/..",
        ] {
            assert_eq!(store.bundle_path(version), None, "{version:?}");
        }
    }
}
