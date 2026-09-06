//! Build and install a CPAN distribution programmatically.

mod distribution;

pub use distribution::{BuildTool, Dependencies, Dependency, Distribution, PhaseDependencies};
pub use perl_wrapper::{ExecuteResult, Perl};
