//! Direct issuance and one-use authorization consumption.

use std::time::Instant;

use crate::features::autoscan::authority::AutoApplyTicket;

use super::super::apply::{
    ApplyAuthorization, ApplyReview, AutoApplyAuthorization, OperationAuthorization,
};
use super::super::challenge::IssuedAuthorization;
use super::super::compare::CompareAuthorization;
use super::issuance::{commit_authorization, issue_into, prepare_authorization, random_token};
use super::retention::purge;
use super::OperationAuthorizationStore;

impl OperationAuthorizationStore {
    pub(crate) fn issue_compare_authorization(
        &self,
        authorization: CompareAuthorization,
    ) -> Result<IssuedAuthorization, String> {
        let mut state = self.0.lock().unwrap();
        let now = Instant::now();
        purge(&mut state, now);
        issue_into(
            &mut state,
            OperationAuthorization::Compare(authorization),
            now,
        )
    }

    pub(crate) fn issue_auto_apply_authorization(
        &self,
        review: ApplyReview,
        ticket: AutoApplyTicket,
    ) -> Result<IssuedAuthorization, String> {
        // Whether AutoScan may apply unattended is `autoscan_auto_apply` in the job, checked where
        // the scan is scheduled. There is no second per-session permission on top of it.
        let authorization = AutoApplyAuthorization::new(review, ticket)?;
        let mut state = self.0.lock().unwrap();
        let now = Instant::now();
        purge(&mut state, now);
        let prepared = prepare_authorization(
            OperationAuthorization::AutoApply(authorization),
            now,
            random_token,
        )?;
        Ok(commit_authorization(&mut state, prepared))
    }

    pub(crate) fn consume_compare_authorization(
        &self,
        token: &str,
    ) -> Result<CompareAuthorization, String> {
        match self.take_authorization(token, Instant::now())? {
            OperationAuthorization::Compare(authorization) => Ok(authorization),
            _ => Err("This operation authorization does not permit Compare".into()),
        }
    }

    pub(crate) fn consume_apply_authorization(
        &self,
        token: &str,
    ) -> Result<ApplyAuthorization, String> {
        match self.take_authorization(token, Instant::now())? {
            OperationAuthorization::InteractiveApply(authorization) => {
                Ok(ApplyAuthorization::Interactive(authorization))
            }
            OperationAuthorization::AutoApply(authorization) => {
                Ok(ApplyAuthorization::AutoScan(authorization))
            }
            OperationAuthorization::Compare(_) => {
                Err("This operation authorization does not permit Apply".into())
            }
        }
    }

    fn take_authorization(
        &self,
        token: &str,
        now: Instant,
    ) -> Result<OperationAuthorization, String> {
        let mut state = self.0.lock().unwrap();
        let index = state
            .authorizations
            .iter()
            .position(|authorization| authorization.token == token)
            .ok_or_else(|| {
                "This operation authorization is invalid, expired, or already used".to_string()
            })?;
        // Burn before expiry or variant inspection, so a wrong command cannot probe and replay it.
        let record = state
            .authorizations
            .remove(index)
            .expect("a located authorization must exist");
        if record.expires <= now {
            return Err("This operation authorization expired — review again".into());
        }
        Ok(record.authorization)
    }
}
