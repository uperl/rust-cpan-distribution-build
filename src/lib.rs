//! Build and install a CPAN distribution programmatically.

mod distribution;
mod perl;

pub use distribution::{BuildTool, Dependencies, Dependency, Distribution, PhaseDependencies};
pub use perl::Perl;
