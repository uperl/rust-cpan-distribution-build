//! A configured Perl interpreter: where `perl` and `make` live, where newly
//! built modules are installed, and which directories go on `PERL5LIB`.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Result, anyhow};

/// A wrapper around a Perl interpreter and the environment used to build and
/// install CPAN distributions with it.
///
/// Construct one with [`Perl::new`] (which finds `perl` on `PATH`) or
/// [`Perl::with_perl`] (which takes an explicit interpreter), then adjust it
/// with [`with_install_base`](Self::with_install_base) and
/// [`with_lib`](Self::with_lib).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Perl {
    /// Full path to the `perl` executable.
    pub perl: PathBuf,

    /// Full path to `make`, or `None` when no `make` is on `PATH`. Required to
    /// build `ExtUtils::MakeMaker` distributions; `Module::Build` distributions
    /// do not need it.
    pub make: Option<PathBuf>,

    /// Full path to the install location for newly installed modules — the
    /// equivalent of `ExtUtils::MakeMaker`'s `INSTALL_BASE` or `Module::Build`'s
    /// `--install_base`. `None` installs to the interpreter's default location.
    pub install_base: Option<PathBuf>,

    /// Full paths of directories to search for Perl modules. Joined with the
    /// platform path separator by [`perl5lib`](Self::perl5lib) to form the
    /// `PERL5LIB` environment variable.
    pub lib: Vec<PathBuf>,
}

impl Perl {
    /// Create a wrapper, resolving `perl` and `make` from `PATH`.
    ///
    /// The first `perl` on `PATH` is used; an error is returned if there is
    /// none. The first `make` on `PATH` is used when present, otherwise
    /// [`make`](Self::make) is `None`. [`install_base`](Self::install_base) is
    /// `None` (install to the default location) and [`lib`](Self::lib) is empty;
    /// set them with [`with_install_base`](Self::with_install_base) and
    /// [`with_lib`](Self::with_lib).
    pub fn new() -> Result<Self> {
        let perl =
            which::which("perl").map_err(|e| anyhow!("no `perl` executable found on PATH: {e}"))?;
        Ok(Self::with_perl(perl))
    }

    /// Create a wrapper that uses `perl` as the interpreter instead of searching
    /// `PATH`.
    ///
    /// `make` is still resolved from `PATH` (first match, or `None`);
    /// [`install_base`](Self::install_base) is `None` and [`lib`](Self::lib) is
    /// empty.
    pub fn with_perl(perl: impl Into<PathBuf>) -> Self {
        Self {
            perl: perl.into(),
            make: which::which("make").ok(),
            install_base: None,
            lib: Vec::new(),
        }
    }

    /// Set the install location for newly installed modules
    /// ([`install_base`](Self::install_base)).
    #[must_use]
    pub fn with_install_base(mut self, install_base: impl Into<PathBuf>) -> Self {
        self.install_base = Some(install_base.into());
        self
    }

    /// Replace the module search path ([`lib`](Self::lib)).
    #[must_use]
    pub fn with_lib<I, P>(mut self, lib: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.lib = lib.into_iter().map(Into::into).collect();
        self
    }

    /// Override [`make`](Self::make) with an explicit path.
    #[must_use]
    pub fn with_make(mut self, make: impl Into<PathBuf>) -> Self {
        self.make = Some(make.into());
        self
    }

    /// The `PERL5LIB` value: [`lib`](Self::lib) joined with the platform path
    /// separator (`:` on Unix, `;` on Windows). Empty when `lib` is empty.
    pub fn perl5lib(&self) -> OsString {
        let separator: &str = if cfg!(windows) { ";" } else { ":" };
        let mut value = OsString::new();
        for (i, dir) in self.lib.iter().enumerate() {
            if i > 0 {
                value.push(separator);
            }
            value.push(dir);
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn new_resolves_perl_from_path() {
        let Ok(perl) = Perl::new() else {
            eprintln!("skipping: no `perl` on PATH");
            return;
        };
        assert!(perl.perl.is_absolute());
        assert!(perl.perl.file_stem().is_some());
        assert!(perl.install_base.is_none());
        assert!(perl.lib.is_empty());
        // `make` mirrors whatever is (or isn't) on PATH.
        assert_eq!(perl.make.is_some(), which::which("make").is_ok());
    }

    #[test]
    fn with_perl_uses_the_given_interpreter() {
        let perl = Perl::with_perl("/usr/local/bin/perl");
        assert_eq!(perl.perl, Path::new("/usr/local/bin/perl"));
        assert!(perl.install_base.is_none());
        assert_eq!(perl.make.is_some(), which::which("make").is_ok());
    }

    #[test]
    fn with_install_base_sets_the_location() {
        let perl = Perl::with_perl("/usr/bin/perl").with_install_base("/opt/perl5");
        assert_eq!(perl.install_base.as_deref(), Some(Path::new("/opt/perl5")));
    }

    #[test]
    fn with_make_overrides_resolution() {
        let perl = Perl::with_perl("/usr/bin/perl").with_make("/usr/bin/gmake");
        assert_eq!(perl.make.as_deref(), Some(Path::new("/usr/bin/gmake")));
    }

    #[test]
    fn with_lib_accepts_various_path_types() {
        let perl = Perl::with_perl("/usr/bin/perl")
            .with_lib(["/opt/perl5/lib/perl5", "/home/me/perl5/lib/perl5"]);
        assert_eq!(
            perl.lib,
            vec![
                PathBuf::from("/opt/perl5/lib/perl5"),
                PathBuf::from("/home/me/perl5/lib/perl5"),
            ]
        );
    }

    #[test]
    #[cfg(unix)]
    fn perl5lib_joins_with_colon() {
        let perl = Perl::with_perl("/usr/bin/perl").with_lib(["/a/lib", "/b/c/lib"]);
        assert_eq!(perl.perl5lib(), OsString::from("/a/lib:/b/c/lib"));
    }

    #[test]
    fn perl5lib_is_empty_without_lib_dirs() {
        let perl = Perl::with_perl("/usr/bin/perl");
        assert_eq!(perl.perl5lib(), OsString::new());
    }
}
