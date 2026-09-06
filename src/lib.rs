//! Build and install a CPAN distribution programmatically.

mod distribution;

pub use distribution::{BuildTool, Dependency, Distribution};
