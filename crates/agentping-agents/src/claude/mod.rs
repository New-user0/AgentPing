//! Claude Code integration via native hooks.

mod detect;
pub mod install;
mod normalize;

pub use detect::{detect, ClaudeDetection};
pub use normalize::{
    hook_output, remote_candidate, ClaudeAdapter, HookOutput, RemoteCandidate,
    ASK_USER_QUESTION_TOOL, INTEGRATION_ID,
};

#[cfg(test)]
mod tests;
