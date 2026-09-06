//! A CPAN distribution unpacked in a local filesystem directory.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use cpan_distribution_meta::Meta;

/// A Perl CPAN distribution laid out in a local filesystem directory.
///
/// Construct one with [`Distribution::new`], passing the path to the directory
/// that contains the distribution (the directory holding `Makefile.PL` /
/// `Build.PL`, `META.json`, and so on). The distribution's static metadata is
/// read and parsed straight away; the generated `MYMETA` metadata only exists
/// once the configure step has produced it, and is read separately with
/// [`Distribution::read_mymeta`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Distribution {
    /// The directory containing the distribution.
    pub root: PathBuf,

    /// Metadata parsed from the distribution's `META.json`, or from `META.yml`
    /// when there is no `META.json`. Read when the [`Distribution`] is created.
    pub distribution_meta: Meta,

    /// Metadata parsed from the distribution's `MYMETA.json`, or from
    /// `MYMETA.yml` when there is no `MYMETA.json`. `MYMETA` files are written
    /// by the configure step (`perl Makefile.PL` / `perl Build.PL`), so this is
    /// `None` until [`Distribution::read_mymeta`] has completed successfully.
    pub distribution_mymeta: Option<Meta>,
}

impl Distribution {
    /// Open the CPAN distribution rooted at `path`.
    ///
    /// `META.json` is read and parsed immediately; if it does not exist,
    /// `META.yml` is used instead. An error is returned if neither file is
    /// present, or if the one that is found cannot be read or parsed.
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let root = path.as_ref().to_path_buf();
        let distribution_meta = read_meta(&root, &["META.json", "META.yml"])?
            .ok_or_else(|| anyhow!("no META.json or META.yml in {}", root.display()))?;
        Ok(Self {
            root,
            distribution_meta,
            distribution_mymeta: None,
        })
    }

    /// Read `MYMETA.json` (or `MYMETA.yml` when there is no `MYMETA.json`) from
    /// the distribution root, parse it, and store it on
    /// [`distribution_mymeta`](Self::distribution_mymeta).
    ///
    /// Returns an error if neither file is present, or if the one that is found
    /// cannot be read or parsed. On success the freshly parsed metadata is
    /// returned for convenience.
    pub fn read_mymeta(&mut self) -> Result<&Meta> {
        let mymeta = read_meta(&self.root, &["MYMETA.json", "MYMETA.yml"])?
            .ok_or_else(|| anyhow!("no MYMETA.json or MYMETA.yml in {}", self.root.display()))?;
        Ok(self.distribution_mymeta.insert(mymeta))
    }
}

/// Return the first of `candidates` that exists as a file in `root`, parsed as
/// CPAN distribution metadata. `Ok(None)` if none of them exist.
fn read_meta(root: &Path, candidates: &[&str]) -> Result<Option<Meta>> {
    for name in candidates {
        let path = root.join(name);
        if path.is_file() {
            let meta = cpan_distribution_meta::from_path(&path)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            return Ok(Some(meta));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use tempfile::TempDir;

    const META_JSON: &str = r#"{
        "name": "Foo-Bar",
        "version": "1.23",
        "abstract": "a foo for bars",
        "author": ["Jane Doe <jane@example.com>"],
        "license": ["perl_5"],
        "dynamic_config": 1,
        "release_status": "stable",
        "meta-spec": { "version": 2 },
        "prereqs": { "runtime": { "requires": { "perl": "5.010" } } }
    }"#;

    const META_YML: &str = "---\nname: Foo-Bar\nversion: '1.23'\nabstract: a foo for bars\nlicense: perl\nrequires:\n  perl: '5.006'\nmeta-spec:\n  version: 1.4\n";

    fn dist_with(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for (name, body) in files {
            fs::write(dir.path().join(name), body).unwrap();
        }
        dir
    }

    #[test]
    fn reads_meta_json_immediately() {
        let dir = dist_with(&[("META.json", META_JSON)]);
        let dist = Distribution::new(dir.path()).unwrap();
        assert_eq!(dist.distribution_meta.name, "Foo-Bar");
        assert_eq!(dist.distribution_meta.version, "1.23");
        assert!(dist.distribution_mymeta.is_none());
    }

    #[test]
    fn prefers_meta_json_over_meta_yml() {
        // The YAML file carries a different spec version; if it were the one
        // parsed, `spec_version` would not be V2.
        let dir = dist_with(&[("META.json", META_JSON), ("META.yml", META_YML)]);
        let dist = Distribution::new(dir.path()).unwrap();
        assert_eq!(
            dist.distribution_meta.spec_version,
            cpan_distribution_meta::SpecVersion::V2
        );
    }

    #[test]
    fn falls_back_to_meta_yml() {
        let dir = dist_with(&[("META.yml", META_YML)]);
        let dist = Distribution::new(dir.path()).unwrap();
        assert_eq!(dist.distribution_meta.name, "Foo-Bar");
        assert_eq!(
            dist.distribution_meta.spec_version,
            cpan_distribution_meta::SpecVersion::V1_4
        );
    }

    #[test]
    fn errors_when_no_meta_present() {
        let dir = dist_with(&[]);
        let err = Distribution::new(dir.path()).unwrap_err();
        assert!(err.to_string().contains("no META.json or META.yml"));
    }

    #[test]
    fn read_mymeta_populates_field() {
        let dir = dist_with(&[("META.json", META_JSON), ("MYMETA.json", META_JSON)]);
        let mut dist = Distribution::new(dir.path()).unwrap();
        assert!(dist.distribution_mymeta.is_none());

        dist.read_mymeta().unwrap();
        assert_eq!(dist.distribution_mymeta.as_ref().unwrap().name, "Foo-Bar");
    }

    #[test]
    fn read_mymeta_errors_when_absent() {
        let dir = dist_with(&[("META.json", META_JSON)]);
        let mut dist = Distribution::new(dir.path()).unwrap();
        let err = dist.read_mymeta().unwrap_err();
        assert!(err.to_string().contains("no MYMETA.json or MYMETA.yml"));
    }
}
