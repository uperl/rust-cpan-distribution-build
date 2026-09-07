# cpan-distribution-build

Build and install a CPAN distribution programmatically.

This crate drives the standard build lifecycle of a Perl CPAN distribution that
has already been unpacked into a local directory. It works out whether the
distribution uses [`ExtUtils::MakeMaker`](https://metacpan.org/pod/ExtUtils::MakeMaker)
(`Makefile.PL`) or [`Module::Build`](https://metacpan.org/pod/Module::Build)
(`Build.PL`), reads the `META.*` / `MYMETA.*` metadata, reports the
prerequisites for each phase, and runs the configure, build, test, and install
steps through a [`Perl`] wrapper.

## Lifecycle

| Step | Method | `ExtUtils::MakeMaker` | `Module::Build` |
| --- | --- | --- | --- |
| Prerequisites needed before configure | `execute_pre_configure` | — | — |
| Configure | `execute_configure` | `perl Makefile.PL` | `perl Build.PL` |
| Build | `execute_build` | `make` | `perl Build` |
| Test | `execute_test` | `make test` | `perl Build test` |
| Install | `execute_install` | `make install` | `perl Build install` |
| Clean | `execute_clean` | `make clean` | `perl Build clean` |

`execute_pre_configure` returns the `configure`-phase requirements from the
metadata (minus `perl`), plus the build tool itself when the metadata does not
already call for it. Installing those is the caller's responsibility.

`execute_configure` returns an `ExecuteResult` together with the distribution's
`Dependencies` grouped by phase (`configure`, `build`, `test`, `runtime`,
`develop`) and relationship (`requires`, `recommends`, `suggests`, `conflicts`).
When the configure step writes a `MYMETA.*` file, the dependencies come from it;
otherwise they come from `META.*`.

A non-zero exit from any step is reported in the returned `ExecuteResult`, not
as an `Err`. An `Err` means `perl` could not be spawned, an expected file was
missing, or a metadata file could not be parsed.

## Usage

```rust
use cpan_distribution_build::{Distribution, Perl};

# fn main() -> anyhow::Result<()> {
// A Perl wrapper; `with_install_base` etc. control where `install` puts files.
let perl = Perl::with_perl("perl");

// Open a distribution already unpacked at this path (the directory that holds
// Makefile.PL / Build.PL, META.json, ...).
let mut dist = Distribution::new("path/to/Foo-Bar-1.23", perl)?;

// Prerequisites that must be present before the configure step can run.
for dep in dist.execute_pre_configure() {
    println!("pre-configure: {} {}", dep.module, dep.version);
}

// Configure, and get the full prerequisite picture back.
let (result, deps) = dist.execute_configure()?;
assert!(result.is_success);
for dep in &deps.runtime.requires {
    println!("runtime: {} {}", dep.module, dep.version);
}

// Build, test, install.
assert!(dist.execute_build()?.is_success);
assert!(dist.execute_test()?.is_success);
assert!(dist.execute_install()?.is_success);
# Ok(())
# }
```

Use `Distribution::with_preference` instead of `Distribution::new` to choose
between `ExtUtils::MakeMaker` and `Module::Build` when a distribution ships both
`Makefile.PL` and `Build.PL`; `Module::Build` is the default.

## Related crates

- [`cpan-distribution-meta`](https://github.com/uperl/rust-cpan-distribution-meta) — parses `META.*` / `MYMETA.*` files.
- [`perl-wrapper`](https://github.com/uperl/rust-perl-wrapper) — runs `perl` and `make` with a CPAN build environment applied.

## License

MIT — see [LICENSE](LICENSE).

[`Perl`]: https://github.com/uperl/rust-perl-wrapper
