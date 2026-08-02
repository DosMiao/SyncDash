//! Backend-owned AutoScan coordination and change-detection workers.

pub(crate) mod controller;
pub(crate) mod model;
pub(crate) mod runtime;
mod state;
pub(crate) mod worker;

#[cfg(test)]
mod tests;
