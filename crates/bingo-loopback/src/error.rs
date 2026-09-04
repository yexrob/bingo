//! One failure type, because a caller's decision is the same whichever half
//! produced it: there is no page to serve, or there is no answer coming.

use thiserror::Error;

use crate::request::MAX_BODY;

#[derive(Debug, Error)]
pub enum LoopbackError {
    #[error("no free loopback port in {0}")]
    NoPort(String),
    #[error("loopback: {0}")]
    Io(String),
    /// What arrived is not a request this server reads. Answerable: the client
    /// is told, and whatever is being served goes on being served.
    #[error("the request is not one this server reads: {0}")]
    Malformed(String),
    #[error("the body is {0} bytes, over the {MAX_BODY} byte limit")]
    TooLarge(usize),
}
