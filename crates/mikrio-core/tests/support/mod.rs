//! Owned process and directory support shared by native primitive probes.

mod directory;
mod process;

pub(super) use directory::TestDirectory;
pub(super) use process::{Probe, process_scenario};
