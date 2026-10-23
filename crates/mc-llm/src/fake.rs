use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::{Llm, LlmError, LlmRequest};

#[derive(Default)]
struct State {
    answers: VecDeque<Result<String, LlmError>>,
    requests: Vec<LlmRequest>,
}

/// Scripted answers in order, and a record of what was asked. Clones share the
/// script, so a test keeps one while the code under test owns another.
#[derive(Clone, Default)]
pub struct FakeLlm {
    state: Arc<Mutex<State>>,
}

impl FakeLlm {
    pub fn new<S: Into<String>>(answers: impl IntoIterator<Item = S>) -> Self {
        Self::scripted(answers.into_iter().map(|a| Ok(a.into())))
    }

    /// Like `new`, with failures in the script too.
    pub fn scripted(answers: impl IntoIterator<Item = Result<String, LlmError>>) -> Self {
        let state = State {
            answers: answers.into_iter().collect(),
            requests: Vec::new(),
        };
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub fn requests(&self) -> Vec<LlmRequest> {
        self.state.lock().expect("fake llm state").requests.clone()
    }
}

impl Llm for FakeLlm {
    fn name(&self) -> String {
        "fake".to_string()
    }

    fn complete(&self, req: &LlmRequest) -> Result<String, LlmError> {
        let mut state = self.state.lock().expect("fake llm state");
        state.requests.push(req.clone());
        state.answers.pop_front().unwrap_or_else(|| {
            Err(LlmError::BadResponse(
                "fake: no scripted answer left".into(),
            ))
        })
    }
}
