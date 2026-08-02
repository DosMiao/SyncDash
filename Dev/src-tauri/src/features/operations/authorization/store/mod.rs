//! Serialized in-process authority store with one-use challenge and token edges.

mod challenge;
mod grant;
mod issuance;
mod retention;
mod revocation;
mod state;
mod token;

#[cfg(test)]
mod tests;

use std::sync::Mutex;

use state::AuthorizationState;

#[derive(Default)]
pub(crate) struct OperationAuthorizationStore(Mutex<AuthorizationState>);
