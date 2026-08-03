//! Compare behavior, split to mirror the decisions the engine makes.

mod conflicts;
mod evidence;
mod fixtures;
mod matching;
mod modes;
mod moves;
mod names;
/// Emits the TypeScript-facing golden vectors, so it runs only under the generation feature.
#[cfg(feature = "export-types")]
mod rule_vectors;
mod symlinks;
mod sync_matrix;
