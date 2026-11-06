#![allow(clippy::all, clippy::pedantic)]

//! `InboxDelivery` implementation of the `Delivery` trait.
//!
//! Delivers attachment review feedback directly through the Inbox server API,
//! which wakes the asking agent with the feedback turn.

use plannotator_tui_schema::inbox::ReplyQuestionRef;

use super::client::InboxClient;
use crate::delivery::{Delivery, DeliveryError};

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct InboxDelivery {
    pub(crate) client: InboxClient,
    pub(crate) message_id: String,
    pub(crate) agent_name: String,
    pub(crate) host: Option<String>,
}

impl Delivery for InboxDelivery {
    fn describe(&self) -> String {
        format!("inbox \u{b7} {}", self.agent_name)
    }

    fn is_agent(&self) -> bool {
        true
    }

    fn agent_host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    fn deliver(&self, feedback: &str) -> Result<(), DeliveryError> {
        self.client
            .send_reply(&self.message_id, "", Vec::<ReplyQuestionRef>::new(), Some(feedback.to_owned()), None)
            .map_err(DeliveryError::Failed)?;
        Ok(())
    }
}
