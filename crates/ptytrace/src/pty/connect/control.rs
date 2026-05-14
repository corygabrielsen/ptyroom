//! Connect-side re-exports of the shared `input_router` types.

#[cfg(test)]
pub(super) use super::super::input_router::LOCAL_ESCAPE;
pub(super) use super::super::input_router::{
    LOCAL_ESCAPE_NAME, LocalInputAction, LocalInputRouter, LocalStatus,
};
