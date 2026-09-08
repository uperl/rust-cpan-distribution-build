//! A CPAN distribution unpacked in a local filesystem directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use cpan_distribution_meta::Meta;

use perl_wrapper::{ExecuteResult, Perl};

/// The build tool a CPAN distribution uses for its configure step.
///
/// A distribution ships either a `Makefile.PL` (driven by
/// [`ExtUtils::MakeMaker`](https://metacpan.org/pod/ExtUtils::MakeMaker)) or a
/// `Build.PL` (driven by [`Module::Build`](https://metacpan.org/pod/Module::Build)
/// or any API-compatible replacement such as `Module::Build::Tiny`); a few ship
/// both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuildTool {
    /// `ExtUtils::MakeMaker`, "EUMM": the configure step is `perl Makefile.PL`.
    Eumm,
    /// `Module::Build`, "MB": the configure step is `perl Build.PL`. Whatever
    /// implementation the distribution's `configure` prerequisites call for is
    /// used as-is (it need not be `Module::Build` itself); classic
    /// `Module::Build` is only assumed when the metadata names none.
    ModuleBuild,
}

impl BuildTool {
    /// The configure script this tool runs: `Makefile.PL` for [`Eumm`], or
    /// `Build.PL` for [`ModuleBuild`].
    ///
    /// [`Eumm`]: BuildTool::Eumm
    /// [`ModuleBuild`]: BuildTool::ModuleBuild
    pub fn configure_script(self) -> &'static str {
        match self {
            BuildTool::Eumm => "Makefile.PL",
            BuildTool::ModuleBuild => "Build.PL",
        }
    }

    /// The configure-step argument that forces a pure-Perl build — no XS is
    /// compiled even where a working compiler and the XS sources are present:
    /// the `PUREPERL_ONLY=1` [`WriteMakefile`] parameter for [`Eumm`], or the
    /// `--pureperl-only` [`Build.PL`] option for [`ModuleBuild`].
    ///
    /// Passing it at configure time is enough: [`ExtUtils::MakeMaker`] bakes it
    /// into the generated `Makefile` and [`Module::Build`] records it in
    /// `_build/`, so the later build / test / install steps honour it without
    /// the argument being repeated.
    ///
    /// [`Eumm`]: BuildTool::Eumm
    /// [`ModuleBuild`]: BuildTool::ModuleBuild
    /// [`WriteMakefile`]: https://metacpan.org/pod/ExtUtils::MakeMaker#PUREPERL_ONLY
    /// [`Build.PL`]: https://metacpan.org/pod/Module::Build::API#%2Fpureperl-only
    /// [`ExtUtils::MakeMaker`]: https://metacpan.org/pod/ExtUtils::MakeMaker
    /// [`Module::Build`]: https://metacpan.org/pod/Module::Build
    pub fn pure_perl_arg(self) -> &'static str {
        match self {
            BuildTool::Eumm => "PUREPERL_ONLY=1",
            BuildTool::ModuleBuild => "--pureperl-only",
        }
    }

    /// The module assumed to provide this tool when the distribution's metadata
    /// does not name one.
    fn fallback_module(self) -> &'static str {
        match self {
            BuildTool::Eumm => "ExtUtils::MakeMaker",
            BuildTool::ModuleBuild => "Module::Build",
        }
    }

    /// Whether `module` is (an implementation of) this build tool, for the
    /// purpose of deciding whether the [`fallback_module`](Self::fallback_module)
    /// still needs to be added to the configure prerequisites.
    fn is_provided_by(self, module: &str) -> bool {
        match self {
            BuildTool::Eumm => module == "ExtUtils::MakeMaker",
            // Any `Module::Build`-family module is treated as an MB provider.
            BuildTool::ModuleBuild => module.starts_with("Module::Build"),
        }
    }
}

impl Default for BuildTool {
    /// [`BuildTool::ModuleBuild`] — MB is preferred when a distribution ships
    /// both `Build.PL` and `Makefile.PL`.
    fn default() -> Self {
        BuildTool::ModuleBuild
    }
}

/// A single prerequisite: a module name and the version range required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    /// Module (package) name, e.g. `ExtUtils::MakeMaker`.
    pub module: String,
    /// Required version range, e.g. `"0"` (any version) or `">= 6.58"`.
    pub version: String,
}

/// Every prerequisite of a distribution, grouped by phase and relationship.
///
/// This mirrors the CPAN Meta Spec `prereqs` structure (see
/// [`cpan_distribution_meta::Prereqs`]), but with each group flattened to a
/// plain list of [`Dependency`]. Build it from a parsed [`Meta`] with
/// [`Dependencies::from_meta`]; [`Distribution::execute_configure`] returns one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dependencies {
    /// Needed to run `Makefile.PL` / `Build.PL`.
    pub configure: PhaseDependencies,
    /// Needed to build the distribution, after the configure step.
    pub build: PhaseDependencies,
    /// Needed to run the test suite.
    pub test: PhaseDependencies,
    /// Needed to use the installed distribution at run time.
    pub runtime: PhaseDependencies,
    /// Needed only to work on the distribution itself.
    pub develop: PhaseDependencies,
}

/// The prerequisites of a single phase, split by relationship.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhaseDependencies {
    /// Hard requirements.
    pub requires: Vec<Dependency>,
    /// Optional; installed by default by most CPAN clients.
    pub recommends: Vec<Dependency>,
    /// Optional; not installed by default.
    pub suggests: Vec<Dependency>,
    /// Module/version combinations known to be incompatible.
    pub conflicts: Vec<Dependency>,
}

impl Dependencies {
    /// Collect the `prereqs` of a parsed [`Meta`] (a `META.*` or `MYMETA.*`
    /// document) into a [`Dependencies`].
    pub fn from_meta(meta: &Meta) -> Self {
        let p = &meta.prereqs;
        Self {
            configure: PhaseDependencies::from_phase(&p.configure),
            build: PhaseDependencies::from_phase(&p.build),
            test: PhaseDependencies::from_phase(&p.test),
            runtime: PhaseDependencies::from_phase(&p.runtime),
            develop: PhaseDependencies::from_phase(&p.develop),
        }
    }
}

impl PhaseDependencies {
    fn from_phase(phase: &cpan_distribution_meta::Phase) -> Self {
        Self {
            requires: deps_from_map(&phase.requires),
            recommends: deps_from_map(&phase.recommends),
            suggests: deps_from_map(&phase.suggests),
            conflicts: deps_from_map(&phase.conflicts),
        }
    }
}

/// Convert a CPAN Meta `module => version-range` map into a sorted list of
/// [`Dependency`].
fn deps_from_map(map: &BTreeMap<String, String>) -> Vec<Dependency> {
    map.iter()
        .map(|(module, version)| Dependency {
            module: module.clone(),
            version: version.clone(),
        })
        .collect()
}

/// A Perl CPAN distribution laid out in a local filesystem directory.
///
/// Construct one with [`Distribution::new`] (or [`Distribution::with_preference`]
/// to choose between EUMM and MB), passing the path to the directory that
/// contains the distribution — the directory holding `Makefile.PL` / `Build.PL`,
/// `META.json`, and so on — together with the [`Perl`] wrapper that
/// [`execute_configure`] (and any later build/install step) will run through.
///
/// On construction the static metadata is read and parsed straight away, and the
/// [`build_tool`](Self::build_tool) is resolved from the scripts present. The
/// supplied [`Perl`] is reconfigured to run in [`root`](Self::root). The
/// generated `MYMETA` metadata only exists once [`execute_configure`] has run;
/// that method loads it, or you can load it on its own with [`read_mymeta`].
///
/// Call [`with_pure_perl`](Self::with_pure_perl) before
/// [`execute_configure`] to force a pure-Perl (no-XS) build.
///
/// [`execute_configure`]: Self::execute_configure
/// [`read_mymeta`]: Self::read_mymeta
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Distribution {
    /// The directory containing the distribution.
    pub root: PathBuf,

    /// The Perl interpreter and environment used to run the distribution's
    /// build steps. Set to run in [`root`](Self::root) on construction.
    pub perl: Perl,

    /// Metadata parsed from the distribution's `META.json`, or from `META.yml`
    /// when there is no `META.json`. Read when the [`Distribution`] is created.
    pub distribution_meta: Meta,

    /// Metadata parsed from the distribution's `MYMETA.json`, or from
    /// `MYMETA.yml` when there is no `MYMETA.json`. `MYMETA` files are written
    /// by the configure step, so this is `None` until [`execute_configure`] or
    /// [`read_mymeta`] has populated it.
    ///
    /// [`execute_configure`]: Self::execute_configure
    /// [`read_mymeta`]: Self::read_mymeta
    pub distribution_mymeta: Option<Meta>,

    /// The build tool the configure step will use, resolved on construction from
    /// the scripts present in [`root`](Self::root) and the preference passed to
    /// [`Distribution::with_preference`].
    pub build_tool: BuildTool,

    /// When `true`, [`execute_configure`](Self::execute_configure) appends the
    /// build tool's [`pure_perl_arg`](BuildTool::pure_perl_arg) so the
    /// distribution is built without compiling XS. `false` (the default) leaves
    /// the choice to the distribution and interpreter. Set with
    /// [`with_pure_perl`](Self::with_pure_perl).
    pub pure_perl: bool,
}

impl Distribution {
    /// Open the CPAN distribution rooted at `path`, to be built with `perl`,
    /// preferring MB ([`BuildTool::ModuleBuild`]) when it ships both `Build.PL`
    /// and `Makefile.PL`.
    ///
    /// `META.json` is read and parsed immediately; if it does not exist,
    /// `META.yml` is used instead. An error is returned if neither metadata file
    /// is present (or the one found cannot be parsed), or if the distribution
    /// has neither `Build.PL` nor `Makefile.PL`.
    pub fn new<P: AsRef<Path>>(path: P, perl: Perl) -> Result<Self> {
        Self::with_preference(path, perl, BuildTool::default())
    }

    /// Open the CPAN distribution rooted at `path`, to be built with `perl`,
    /// choosing `prefer` as the build tool when it ships **both** `Build.PL` and
    /// `Makefile.PL`.
    ///
    /// When only one of the two scripts is present, that one is used regardless
    /// of `prefer`. Metadata handling and the error cases are as for
    /// [`Distribution::new`].
    pub fn with_preference<P: AsRef<Path>>(path: P, perl: Perl, prefer: BuildTool) -> Result<Self> {
        let root = path.as_ref().to_path_buf();
        let distribution_meta = read_meta(&root, &["META.json", "META.yml"])?
            .ok_or_else(|| anyhow!("no META.json or META.yml in {}", root.display()))?;

        let has_build_pl = root.join("Build.PL").is_file();
        let has_makefile_pl = root.join("Makefile.PL").is_file();
        let build_tool = match (has_build_pl, has_makefile_pl) {
            (true, true) => prefer,
            (true, false) => BuildTool::ModuleBuild,
            (false, true) => BuildTool::Eumm,
            (false, false) => bail!("no Build.PL or Makefile.PL in {}", root.display()),
        };

        Ok(Self {
            perl: perl.with_current_dir(&root),
            root,
            distribution_meta,
            distribution_mymeta: None,
            build_tool,
            pure_perl: false,
        })
    }

    /// Set whether the configure step forces a pure-Perl, no-XS build
    /// ([`pure_perl`](Self::pure_perl)).
    ///
    /// With it enabled, [`execute_configure`](Self::execute_configure) passes
    /// the build tool's [`pure_perl_arg`](BuildTool::pure_perl_arg)
    /// (`PUREPERL_ONLY=1` for `ExtUtils::MakeMaker`, `--pureperl-only` for
    /// `Module::Build`) as a configure-step argument — no environment variable
    /// is set, and the later steps need no further flagging.
    #[must_use]
    pub fn with_pure_perl(mut self, pure_perl: bool) -> Self {
        self.pure_perl = pure_perl;
        self
    }

    /// The prerequisites that must be installed **before** the configure step
    /// can run.
    ///
    /// This is the distribution's `configure`-phase `requires` (from
    /// [`distribution_meta`](Self::distribution_meta)), plus the build tool
    /// itself ([`ExtUtils::MakeMaker`] or [`Module::Build`]) when the metadata
    /// does not already call for it. `perl` is dropped — it is a
    /// minimum-version marker, not an installable module.
    ///
    /// Installing the returned dependencies is the caller's responsibility; this
    /// method only computes the list. The result is sorted by module name.
    ///
    /// [`ExtUtils::MakeMaker`]: https://metacpan.org/pod/ExtUtils::MakeMaker
    /// [`Module::Build`]: https://metacpan.org/pod/Module::Build
    pub fn execute_pre_configure(&self) -> Vec<Dependency> {
        let mut deps: BTreeMap<String, String> = self
            .distribution_meta
            .prereqs
            .configure
            .requires
            .iter()
            .filter(|(module, _)| module.as_str() != "perl")
            .map(|(module, version)| (module.clone(), version.clone()))
            .collect();

        if !deps
            .keys()
            .any(|module| self.build_tool.is_provided_by(module))
        {
            deps.entry(self.build_tool.fallback_module().to_string())
                .or_insert_with(|| "0".to_string());
        }

        deps.into_iter()
            .map(|(module, version)| Dependency { module, version })
            .collect()
    }

    /// Run the configure step: `perl Build.PL` for [`BuildTool::ModuleBuild`], or
    /// `perl Makefile.PL` for [`BuildTool::Eumm`], through [`perl`](Self::perl)
    /// (which runs it in [`root`](Self::root) with the CPAN build environment
    /// applied).
    ///
    /// Returns the [`ExecuteResult`] paired with the distribution's
    /// [`Dependencies`]. A non-zero exit is reported in the [`ExecuteResult`],
    /// not as an error; an `Err` means `perl` could not be spawned, the script
    /// is missing, or a produced `MYMETA.*` could not be parsed.
    ///
    /// The configure step normally writes a `MYMETA.json` / `MYMETA.yml` with the
    /// prerequisites resolved for the current environment. If one is present
    /// afterwards it is parsed into [`distribution_mymeta`](Self::distribution_mymeta)
    /// and the returned [`Dependencies`] come from it; otherwise they come from
    /// [`distribution_meta`](Self::distribution_meta).
    ///
    /// When [`pure_perl`](Self::pure_perl) is set the build tool's
    /// [`pure_perl_arg`](BuildTool::pure_perl_arg) is appended to the script's
    /// arguments, so the distribution is built without XS.
    pub fn execute_configure(&mut self) -> Result<(ExecuteResult, Dependencies)> {
        let script = self.build_tool.configure_script();
        if !self.root.join(script).is_file() {
            bail!("{script} not found in {}", self.root.display());
        }

        let mut args = vec![script];
        if self.pure_perl {
            args.push(self.build_tool.pure_perl_arg());
        }
        let result = self.perl.execute_perl(args)?;

        if let Some(mymeta) = read_meta(&self.root, &["MYMETA.json", "MYMETA.yml"])? {
            self.distribution_mymeta = Some(mymeta);
        }

        let dependencies = Dependencies::from_meta(
            self.distribution_mymeta
                .as_ref()
                .unwrap_or(&self.distribution_meta),
        );

        Ok((result, dependencies))
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

    /// Run the build step through [`perl`](Self::perl) (in [`root`](Self::root),
    /// with the CPAN build environment applied): `make` for
    /// [`BuildTool::Eumm`], or `perl Build` for [`BuildTool::ModuleBuild`].
    ///
    /// This runs after [`execute_configure`](Self::execute_configure), which
    /// generates the `Makefile` / `Build` script invoked here; an error is
    /// returned if that artifact is missing (or, for EUMM, if no `make` is
    /// available). A non-zero exit is reported in the returned [`ExecuteResult`],
    /// not as an error.
    pub fn execute_build(&self) -> Result<ExecuteResult> {
        self.run_build_target(None)
    }

    /// Run the test suite through [`perl`](Self::perl): `make test` for
    /// [`BuildTool::Eumm`], or `perl Build test` for [`BuildTool::ModuleBuild`].
    ///
    /// Runs after [`execute_build`](Self::execute_build); the requirements and
    /// error behaviour are the same as for [`execute_build`](Self::execute_build).
    pub fn execute_test(&self) -> Result<ExecuteResult> {
        self.run_build_target(Some("test"))
    }

    /// Install the built distribution through [`perl`](Self::perl): `make
    /// install` for [`BuildTool::Eumm`], or `perl Build install` for
    /// [`BuildTool::ModuleBuild`].
    ///
    /// Where files land is governed by the [`perl`](Self::perl) wrapper's
    /// `install_base`. Runs after [`execute_build`](Self::execute_build); the
    /// requirements and error behaviour are the same as for
    /// [`execute_build`](Self::execute_build).
    pub fn execute_install(&self) -> Result<ExecuteResult> {
        self.run_build_target(Some("install"))
    }

    /// Undo the build step through [`perl`](Self::perl): `make clean` for
    /// [`BuildTool::Eumm`], or `perl Build clean` for [`BuildTool::ModuleBuild`].
    ///
    /// This removes the files created by [`execute_build`](Self::execute_build)
    /// (object files, the `blib/` tree, and so on) while leaving the generated
    /// `Makefile` / `Build` script in place. Runs after
    /// [`execute_configure`](Self::execute_configure); the requirements and error
    /// behaviour are the same as for [`execute_build`](Self::execute_build).
    pub fn execute_clean(&self) -> Result<ExecuteResult> {
        self.run_build_target(Some("clean"))
    }

    /// Undo the configure step too, through [`perl`](Self::perl): `make
    /// distclean` for [`BuildTool::Eumm`], or `perl Build distclean` for
    /// [`BuildTool::ModuleBuild`].
    ///
    /// Like [`execute_clean`](Self::execute_clean), but also removes the
    /// generated `Makefile` / `Build` script and the `MYMETA.*` files, returning
    /// the tree to roughly its pre-[`execute_configure`](Self::execute_configure)
    /// state. The generated script still has to be present to run this, so it
    /// must be called before [`execute_clean`](Self::execute_clean); the
    /// requirements and error behaviour are otherwise as for
    /// [`execute_build`](Self::execute_build).
    pub fn execute_distclean(&self) -> Result<ExecuteResult> {
        self.run_build_target(Some("distclean"))
    }

    /// Shared driver for [`execute_build`](Self::execute_build) and
    /// [`execute_test`](Self::execute_test): invoke the generated build script
    /// with an optional target (`None` builds the default target).
    fn run_build_target(&self, target: Option<&str>) -> Result<ExecuteResult> {
        match self.build_tool {
            BuildTool::Eumm => {
                if !self.root.join("Makefile").is_file() {
                    bail!(
                        "Makefile not found in {}; run execute_configure first",
                        self.root.display()
                    );
                }
                self.perl.execute_make(target)
            }
            BuildTool::ModuleBuild => {
                if !self.root.join("Build").is_file() {
                    bail!(
                        "Build not found in {}; run execute_configure first",
                        self.root.display()
                    );
                }
                let args: Vec<&str> = std::iter::once("Build").chain(target).collect();
                self.perl.execute_perl(args)
            }
        }
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
    use std::process::Command;

    use tempfile::TempDir;

    /// A `Perl` wrapper for tests that never actually runs `perl` (or whose test
    /// is `perl`-gated). `with_perl` does not require the interpreter to exist.
    fn test_perl() -> Perl {
        Perl::with_perl("perl")
    }

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

    const META_MULTI_PHASE: &str = r#"{
        "name": "Foo-Bar",
        "version": "1.23",
        "dynamic_config": 0,
        "release_status": "stable",
        "meta-spec": { "version": 2 },
        "prereqs": {
            "configure": { "requires": { "ExtUtils::MakeMaker": "0" } },
            "runtime": {
                "requires": { "perl": "5.010", "Carp": "0" },
                "recommends": { "JSON::XS": "3.0" }
            },
            "test": { "requires": { "Test::More": "0.88" } }
        }
    }"#;

    fn dep(module: &str, version: &str) -> Dependency {
        Dependency {
            module: module.into(),
            version: version.into(),
        }
    }

    /// A `META.json` with the given `configure.requires` map body, e.g.
    /// `r#""Module::Build::Tiny": "0.034", "perl": "5.010""#`.
    fn meta_with_configure(requires_body: &str) -> String {
        format!(
            r#"{{
                "name": "Foo-Bar",
                "version": "1.23",
                "dynamic_config": 1,
                "release_status": "stable",
                "meta-spec": {{ "version": 2 }},
                "prereqs": {{ "configure": {{ "requires": {{ {requires_body} }} }} }}
            }}"#
        )
    }

    fn dist_with(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for (name, body) in files {
            fs::write(dir.path().join(name), body).unwrap();
        }
        dir
    }

    fn perl_available() -> bool {
        Command::new("perl")
            .arg("-e0")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn reads_meta_json_immediately() {
        let dir = dist_with(&[("META.json", META_JSON), ("Build.PL", "1;\n")]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.distribution_meta.name, "Foo-Bar");
        assert_eq!(dist.distribution_meta.version, "1.23");
        assert!(dist.distribution_mymeta.is_none());
    }

    #[test]
    fn construction_points_perl_at_the_distribution_root() {
        let dir = dist_with(&[("META.json", META_JSON), ("Build.PL", "1;\n")]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.perl.current_dir.as_deref(), Some(dir.path()));
    }

    #[test]
    fn prefers_meta_json_over_meta_yml() {
        // The YAML file carries a different spec version; if it were the one
        // parsed, `spec_version` would not be V2.
        let dir = dist_with(&[
            ("META.json", META_JSON),
            ("META.yml", META_YML),
            ("Build.PL", "1;\n"),
        ]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(
            dist.distribution_meta.spec_version,
            cpan_distribution_meta::SpecVersion::V2
        );
    }

    #[test]
    fn falls_back_to_meta_yml() {
        let dir = dist_with(&[("META.yml", META_YML), ("Makefile.PL", "1;\n")]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.distribution_meta.name, "Foo-Bar");
        assert_eq!(
            dist.distribution_meta.spec_version,
            cpan_distribution_meta::SpecVersion::V1_4
        );
    }

    #[test]
    fn errors_when_no_meta_present() {
        let dir = dist_with(&[("Build.PL", "1;\n")]);
        let err = Distribution::new(dir.path(), test_perl()).unwrap_err();
        assert!(err.to_string().contains("no META.json or META.yml"));
    }

    #[test]
    fn errors_when_no_configure_script() {
        let dir = dist_with(&[("META.json", META_JSON)]);
        let err = Distribution::new(dir.path(), test_perl()).unwrap_err();
        assert!(err.to_string().contains("no Build.PL or Makefile.PL"));
    }

    #[test]
    fn defaults_to_module_build_when_both_scripts_present() {
        let dir = dist_with(&[
            ("META.json", META_JSON),
            ("Build.PL", "1;\n"),
            ("Makefile.PL", "1;\n"),
        ]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.build_tool, BuildTool::ModuleBuild);
    }

    #[test]
    fn preference_selects_eumm_when_both_scripts_present() {
        let dir = dist_with(&[
            ("META.json", META_JSON),
            ("Build.PL", "1;\n"),
            ("Makefile.PL", "1;\n"),
        ]);
        let dist = Distribution::with_preference(dir.path(), test_perl(), BuildTool::Eumm).unwrap();
        assert_eq!(dist.build_tool, BuildTool::Eumm);
    }

    #[test]
    fn single_script_wins_over_preference() {
        let dir = dist_with(&[("META.json", META_JSON), ("Makefile.PL", "1;\n")]);
        let dist =
            Distribution::with_preference(dir.path(), test_perl(), BuildTool::ModuleBuild).unwrap();
        assert_eq!(dist.build_tool, BuildTool::Eumm);

        let dir = dist_with(&[("META.json", META_JSON), ("Build.PL", "1;\n")]);
        let dist = Distribution::with_preference(dir.path(), test_perl(), BuildTool::Eumm).unwrap();
        assert_eq!(dist.build_tool, BuildTool::ModuleBuild);
    }

    #[test]
    fn pre_configure_lists_configure_requires_without_perl_and_adds_tool() {
        let meta = meta_with_configure(r#""File::Which": "1.09", "perl": "5.010""#);
        let dir = dist_with(&[("META.json", &meta), ("Build.PL", "1;\n")]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();

        let deps = dist.execute_pre_configure();
        assert_eq!(
            deps,
            vec![
                Dependency {
                    module: "File::Which".into(),
                    version: "1.09".into()
                },
                Dependency {
                    module: "Module::Build".into(),
                    version: "0".into()
                },
            ]
        );
    }

    #[test]
    fn pre_configure_keeps_named_mb_implementation() {
        let meta = meta_with_configure(r#""Module::Build::Tiny": "0.034""#);
        let dir = dist_with(&[("META.json", &meta), ("Build.PL", "1;\n")]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();

        let deps = dist.execute_pre_configure();
        assert_eq!(
            deps,
            vec![Dependency {
                module: "Module::Build::Tiny".into(),
                version: "0.034".into()
            }]
        );
    }

    #[test]
    fn pre_configure_adds_eumm_for_makefile_pl() {
        let dir = dist_with(&[("META.json", META_JSON), ("Makefile.PL", "1;\n")]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();

        let deps = dist.execute_pre_configure();
        assert_eq!(
            deps,
            vec![Dependency {
                module: "ExtUtils::MakeMaker".into(),
                version: "0".into()
            }]
        );
    }

    #[test]
    fn dependencies_from_meta_group_by_phase_and_relationship() {
        let dir = dist_with(&[("META.json", META_MULTI_PHASE), ("Makefile.PL", "1;\n")]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();

        let deps = Dependencies::from_meta(&dist.distribution_meta);
        // Sorted by module name; `perl` is kept (this is the full picture).
        assert_eq!(
            deps.runtime.requires,
            vec![dep("Carp", "0"), dep("perl", "5.010")]
        );
        assert_eq!(deps.runtime.recommends, vec![dep("JSON::XS", "3.0")]);
        assert_eq!(deps.test.requires, vec![dep("Test::More", "0.88")]);
        assert_eq!(
            deps.configure.requires,
            vec![dep("ExtUtils::MakeMaker", "0")]
        );
        assert!(deps.build.requires.is_empty());
        assert!(deps.develop.requires.is_empty());
        assert!(deps.runtime.suggests.is_empty());
    }

    #[test]
    fn execute_configure_populates_mymeta_and_returns_its_deps() {
        if !perl_available() {
            eprintln!("skipping: no `perl` on PATH");
            return;
        }
        // Build.PL writes a MYMETA.json with prereqs that differ from META.json.
        let build_pl = "open my $fh, '>', 'MYMETA.json' or die $!;\n\
             print {$fh} '{\"name\":\"Foo-Bar\",\"version\":\"1.23\",\"meta-spec\":{\"version\":2},\
             \"prereqs\":{\"runtime\":{\"requires\":{\"Moo\":\"2.0\"}}}}';\n\
             close $fh;\n";
        let dir = dist_with(&[("META.json", META_JSON), ("Build.PL", build_pl)]);
        let mut dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.build_tool, BuildTool::ModuleBuild);

        let (result, deps) = dist.execute_configure().unwrap();
        assert!(result.is_success);
        assert_eq!(dist.distribution_mymeta.as_ref().unwrap().name, "Foo-Bar");
        assert_eq!(deps.runtime.requires, vec![dep("Moo", "2.0")]);
    }

    #[test]
    fn execute_configure_without_mymeta_falls_back_to_distribution_meta() {
        if !perl_available() {
            eprintln!("skipping: no `perl` on PATH");
            return;
        }
        let dir = dist_with(&[("META.json", META_MULTI_PHASE), ("Makefile.PL", "1;\n")]);
        let mut dist = Distribution::new(dir.path(), test_perl()).unwrap();

        let (result, deps) = dist.execute_configure().unwrap();
        assert!(result.is_success);
        assert!(dist.distribution_mymeta.is_none());
        assert_eq!(deps.test.requires, vec![dep("Test::More", "0.88")]);
    }

    #[test]
    fn execute_configure_reports_script_failure_in_result() {
        if !perl_available() {
            eprintln!("skipping: no `perl` on PATH");
            return;
        }
        let dir = dist_with(&[
            ("META.json", META_JSON),
            ("Makefile.PL", "die \"boom\\n\";\n"),
        ]);
        let mut dist = Distribution::new(dir.path(), test_perl()).unwrap();

        // A failing script is not an error; it is reported in the result, and
        // dependencies fall back to distribution_meta.
        let (result, deps) = dist.execute_configure().unwrap();
        assert!(!result.is_success);
        assert!(dist.distribution_mymeta.is_none());
        assert_eq!(deps.runtime.requires, vec![dep("perl", "5.010")]);
    }

    #[test]
    fn pure_perl_arg_is_the_configure_step_flag_for_each_tool() {
        assert_eq!(BuildTool::Eumm.pure_perl_arg(), "PUREPERL_ONLY=1");
        assert_eq!(BuildTool::ModuleBuild.pure_perl_arg(), "--pureperl-only");
    }

    #[test]
    fn with_pure_perl_toggles_the_flag_off_by_default() {
        let dir = dist_with(&[("META.json", META_JSON), ("Build.PL", "1;\n")]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert!(!dist.pure_perl);
        assert!(dist.clone().with_pure_perl(true).pure_perl);
        assert!(!dist.with_pure_perl(true).with_pure_perl(false).pure_perl);
    }

    /// A configure script that records its `@ARGV`, one entry per line, in
    /// `argv.txt` beside itself.
    const DUMP_ARGV: &str =
        "open my $fh, '>', 'argv.txt' or die $!; print {$fh} join qq(\\n), @ARGV; close $fh;\n";

    #[test]
    fn execute_configure_passes_pureperl_only_to_makefile_pl() {
        if !perl_available() {
            eprintln!("skipping: no `perl` on PATH");
            return;
        }
        let dir = dist_with(&[("META.json", META_JSON), ("Makefile.PL", DUMP_ARGV)]);
        let mut dist = Distribution::new(dir.path(), test_perl())
            .unwrap()
            .with_pure_perl(true);
        assert_eq!(dist.build_tool, BuildTool::Eumm);

        assert!(dist.execute_configure().unwrap().0.is_success);
        let argv = fs::read_to_string(dir.path().join("argv.txt")).unwrap();
        assert_eq!(argv.lines().collect::<Vec<_>>(), ["PUREPERL_ONLY=1"]);
    }

    #[test]
    fn execute_configure_passes_pureperl_only_to_build_pl() {
        if !perl_available() {
            eprintln!("skipping: no `perl` on PATH");
            return;
        }
        let dir = dist_with(&[("META.json", META_JSON), ("Build.PL", DUMP_ARGV)]);
        let mut dist = Distribution::new(dir.path(), test_perl())
            .unwrap()
            .with_pure_perl(true);
        assert_eq!(dist.build_tool, BuildTool::ModuleBuild);

        assert!(dist.execute_configure().unwrap().0.is_success);
        let argv = fs::read_to_string(dir.path().join("argv.txt")).unwrap();
        assert_eq!(argv.lines().collect::<Vec<_>>(), ["--pureperl-only"]);
    }

    #[test]
    fn execute_configure_passes_no_extra_args_by_default() {
        if !perl_available() {
            eprintln!("skipping: no `perl` on PATH");
            return;
        }
        let dir = dist_with(&[("META.json", META_JSON), ("Build.PL", DUMP_ARGV)]);
        let mut dist = Distribution::new(dir.path(), test_perl()).unwrap();

        assert!(dist.execute_configure().unwrap().0.is_success);
        let argv = fs::read_to_string(dir.path().join("argv.txt")).unwrap();
        assert!(argv.is_empty(), "expected no configure args, got {argv:?}");
    }

    #[test]
    fn execute_build_errors_before_configure() {
        let dir = dist_with(&[("META.json", META_JSON), ("Build.PL", "1;\n")]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.build_tool, BuildTool::ModuleBuild);
        assert!(
            dist.execute_build()
                .unwrap_err()
                .to_string()
                .contains("Build not found")
        );

        let dir = dist_with(&[("META.json", META_JSON), ("Makefile.PL", "1;\n")]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.build_tool, BuildTool::Eumm);
        assert!(
            dist.execute_build()
                .unwrap_err()
                .to_string()
                .contains("Makefile not found")
        );
    }

    #[test]
    fn execute_build_runs_perl_build_for_module_build() {
        if !perl_available() {
            eprintln!("skipping: no `perl` on PATH");
            return;
        }
        let dir = dist_with(&[
            ("META.json", META_JSON),
            ("Build.PL", "1;\n"),
            ("Build", "print \"built\\n\"; exit 0;\n"),
        ]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.build_tool, BuildTool::ModuleBuild);

        let result = dist.execute_build().unwrap();
        assert!(result.is_success);
        assert_eq!(result.code, Some(0));
    }

    #[test]
    fn execute_build_runs_make_for_eumm() {
        if which::which("make").is_err() {
            eprintln!("skipping: no `make` on PATH");
            return;
        }
        let dir = dist_with(&[
            ("META.json", META_JSON),
            ("Makefile.PL", "1;\n"),
            ("Makefile", "all:\n\t@true\n"),
        ]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.build_tool, BuildTool::Eumm);

        let result = dist.execute_build().unwrap();
        assert!(result.is_success);
    }

    #[test]
    fn execute_test_passes_the_test_target_for_module_build() {
        if !perl_available() {
            eprintln!("skipping: no `perl` on PATH");
            return;
        }
        // `perl Build test` -> ok; `perl Build` -> fails.
        let dir = dist_with(&[
            ("META.json", META_JSON),
            ("Build.PL", "1;\n"),
            ("Build", "exit(($ARGV[0] // '') eq 'test' ? 0 : 1);\n"),
        ]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.build_tool, BuildTool::ModuleBuild);

        assert!(dist.execute_test().unwrap().is_success);
        assert!(!dist.execute_build().unwrap().is_success);
    }

    #[test]
    fn execute_test_runs_make_test_for_eumm() {
        if which::which("make").is_err() {
            eprintln!("skipping: no `make` on PATH");
            return;
        }
        // `make` (default target) fails; `make test` succeeds.
        let dir = dist_with(&[
            ("META.json", META_JSON),
            ("Makefile.PL", "1;\n"),
            ("Makefile", "all:\n\t@false\ntest:\n\t@true\n"),
        ]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.build_tool, BuildTool::Eumm);

        assert!(dist.execute_test().unwrap().is_success);
        assert!(!dist.execute_build().unwrap().is_success);
    }

    #[test]
    fn execute_install_passes_the_install_target_for_module_build() {
        if !perl_available() {
            eprintln!("skipping: no `perl` on PATH");
            return;
        }
        let dir = dist_with(&[
            ("META.json", META_JSON),
            ("Build.PL", "1;\n"),
            ("Build", "exit(($ARGV[0] // '') eq 'install' ? 0 : 1);\n"),
        ]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.build_tool, BuildTool::ModuleBuild);

        assert!(dist.execute_install().unwrap().is_success);
        assert!(!dist.execute_build().unwrap().is_success);
    }

    #[test]
    fn execute_install_runs_make_install_for_eumm() {
        if which::which("make").is_err() {
            eprintln!("skipping: no `make` on PATH");
            return;
        }
        let dir = dist_with(&[
            ("META.json", META_JSON),
            ("Makefile.PL", "1;\n"),
            ("Makefile", "all:\n\t@false\ninstall:\n\t@true\n"),
        ]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.build_tool, BuildTool::Eumm);

        assert!(dist.execute_install().unwrap().is_success);
    }

    #[test]
    fn execute_clean_passes_the_clean_target_for_module_build() {
        if !perl_available() {
            eprintln!("skipping: no `perl` on PATH");
            return;
        }
        let dir = dist_with(&[
            ("META.json", META_JSON),
            ("Build.PL", "1;\n"),
            ("Build", "exit(($ARGV[0] // '') eq 'clean' ? 0 : 1);\n"),
        ]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.build_tool, BuildTool::ModuleBuild);

        assert!(dist.execute_clean().unwrap().is_success);
        assert!(!dist.execute_build().unwrap().is_success);
    }

    #[test]
    fn execute_clean_runs_make_clean_for_eumm() {
        if which::which("make").is_err() {
            eprintln!("skipping: no `make` on PATH");
            return;
        }
        let dir = dist_with(&[
            ("META.json", META_JSON),
            ("Makefile.PL", "1;\n"),
            ("Makefile", "all:\n\t@false\nclean:\n\t@true\n"),
        ]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.build_tool, BuildTool::Eumm);

        assert!(dist.execute_clean().unwrap().is_success);
        assert!(!dist.execute_build().unwrap().is_success);
    }

    #[test]
    fn execute_clean_errors_before_configure() {
        let dir = dist_with(&[("META.json", META_JSON), ("Build.PL", "1;\n")]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert!(
            dist.execute_clean()
                .unwrap_err()
                .to_string()
                .contains("Build not found")
        );
    }

    #[test]
    fn execute_distclean_passes_the_distclean_target_for_module_build() {
        if !perl_available() {
            eprintln!("skipping: no `perl` on PATH");
            return;
        }
        let dir = dist_with(&[
            ("META.json", META_JSON),
            ("Build.PL", "1;\n"),
            ("Build", "exit(($ARGV[0] // '') eq 'distclean' ? 0 : 1);\n"),
        ]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.build_tool, BuildTool::ModuleBuild);

        assert!(dist.execute_distclean().unwrap().is_success);
        assert!(!dist.execute_build().unwrap().is_success);
    }

    #[test]
    fn execute_distclean_runs_make_distclean_for_eumm() {
        if which::which("make").is_err() {
            eprintln!("skipping: no `make` on PATH");
            return;
        }
        let dir = dist_with(&[
            ("META.json", META_JSON),
            ("Makefile.PL", "1;\n"),
            ("Makefile", "all:\n\t@false\ndistclean:\n\t@true\n"),
        ]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert_eq!(dist.build_tool, BuildTool::Eumm);

        assert!(dist.execute_distclean().unwrap().is_success);
        assert!(!dist.execute_build().unwrap().is_success);
    }

    #[test]
    fn execute_distclean_errors_before_configure() {
        let dir = dist_with(&[("META.json", META_JSON), ("Build.PL", "1;\n")]);
        let dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert!(
            dist.execute_distclean()
                .unwrap_err()
                .to_string()
                .contains("Build not found")
        );
    }

    #[test]
    fn read_mymeta_populates_field() {
        let dir = dist_with(&[
            ("META.json", META_JSON),
            ("MYMETA.json", META_JSON),
            ("Build.PL", "1;\n"),
        ]);
        let mut dist = Distribution::new(dir.path(), test_perl()).unwrap();
        assert!(dist.distribution_mymeta.is_none());

        dist.read_mymeta().unwrap();
        assert_eq!(dist.distribution_mymeta.as_ref().unwrap().name, "Foo-Bar");
    }

    #[test]
    fn read_mymeta_errors_when_absent() {
        let dir = dist_with(&[("META.json", META_JSON), ("Build.PL", "1;\n")]);
        let mut dist = Distribution::new(dir.path(), test_perl()).unwrap();
        let err = dist.read_mymeta().unwrap_err();
        assert!(err.to_string().contains("no MYMETA.json or MYMETA.yml"));
    }
}
