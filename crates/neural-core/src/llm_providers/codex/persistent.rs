//! Private ownership of persistent Codex app-server turns for both provider interfaces.
//! Completion and streaming share process, thread, deadline, and failure cleanup.
use super::{CodexAppServerSession, CodexProvider, CodexTurnOutput, codex_mcp_stream_key};
use crate::error::{NeuralError, Result};
use crate::llm_providers::LlmRequest;
use crate::llm_providers::contract::{LlmStreamEventSink, ProviderCallControl};
use crate::process::StreamControl;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

type CodexSessionSlot = Mutex<Option<CodexAppServerSession>>;
pub(super) type CodexSessions = HashMap<String, Arc<CodexSessionSlot>>;

pub(super) struct CodexInvocation<'a> {
    started: Instant,
    timeout: Duration,
    caller: Option<&'a dyn ProviderCallControl>,
    pub(super) stream: StreamControl,
}

impl<'a> CodexInvocation<'a> {
    fn new(
        timeout: Duration,
        caller: Option<&'a dyn ProviderCallControl>,
        stream: StreamControl,
    ) -> Self {
        Self {
            started: Instant::now(),
            // Background assistant turns have their own provider budget. The
            // caller still enforces a shorter foreground invocation deadline.
            timeout,
            caller,
            stream,
        }
    }

    pub(super) fn check(&self, command: &str) -> Result<()> {
        if self.stream.is_cancelled() || self.caller.is_some_and(|caller| caller.is_cancelled()) {
            return Err(NeuralError::ProcessCancelled {
                command: command.into(),
            });
        }
        if self.started.elapsed() >= self.timeout
            || self.caller.is_some_and(|caller| caller.is_expired())
        {
            return Err(NeuralError::ProcessTimeout {
                command: command.into(),
                timeout_ms: self.timeout.as_millis() as u64,
            });
        }
        Ok(())
    }
}

impl CodexProvider {
    pub(super) fn app_server_turn(
        &self,
        request: &LlmRequest,
        caller: Option<&dyn ProviderCallControl>,
        stream: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<CodexTurnOutput> {
        let timeout = self
            .config
            .spawn
            .as_ref()
            .ok_or_else(|| super::missing_spawn("codex"))?
            .timeout();
        let invocation = CodexInvocation::new(timeout, caller, stream);
        let key = codex_mcp_stream_key(request);
        let slot = self.app_server_session_slot(&key)?;
        let mut session = self.lock_app_server_session(&slot, &invocation)?;
        self.prepare_app_server_session(&mut session, request, &invocation)?;
        let active = session.as_mut().ok_or_else(|| missing_session(&key))?;
        let result = active.run_turn(request, &invocation, on_event);
        if result.is_err() {
            session.take();
        }
        result
    }

    fn app_server_session_slot(&self, key: &str) -> Result<Arc<CodexSessionSlot>> {
        // Only conversations sharing a scoped MCP connection share a process.
        // Never retain the registry lock during provider I/O or process cleanup.
        let mut sessions = self
            .app_server_sessions
            .lock()
            .map_err(|_| missing_session("poisoned session registry"))?;
        Ok(Arc::clone(
            sessions
                .entry(key.into())
                .or_insert_with(|| Arc::new(Mutex::new(None))),
        ))
    }

    fn lock_app_server_session<'a>(
        &self,
        slot: &'a CodexSessionSlot,
        invocation: &CodexInvocation<'_>,
    ) -> Result<MutexGuard<'a, Option<CodexAppServerSession>>> {
        loop {
            invocation.check(&self.config.provider_id)?;
            match slot.try_lock() {
                Ok(session) => return Ok(session),
                Err(TryLockError::Poisoned(_)) => {
                    return Err(missing_session("poisoned Codex conversation"));
                }
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    fn prepare_app_server_session(
        &self,
        session: &mut Option<CodexAppServerSession>,
        request: &LlmRequest,
        invocation: &CodexInvocation<'_>,
    ) -> Result<()> {
        if session
            .as_ref()
            .is_some_and(|session| session.is_compatible(&self.config, request))
        {
            return Ok(());
        }
        session.take();
        *session = Some(CodexAppServerSession::spawn(
            &self.config,
            request,
            invocation,
        )?);
        Ok(())
    }
}

fn missing_session(value: &str) -> NeuralError {
    NeuralError::InvalidValue {
        value: value.into(),
        expected: "available persistent Codex app-server session".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_turn_uses_configured_deadline_beyond_sixty_seconds() {
        let mut invocation =
            CodexInvocation::new(Duration::from_secs(180), None, StreamControl::unbounded());
        invocation.started = Instant::now() - Duration::from_secs(61);
        assert!(
            invocation.check("codex").is_ok(),
            "active background turn must survive 60s"
        );
        invocation.started = Instant::now() - Duration::from_secs(181);
        assert!(matches!(
            invocation.check("codex"),
            Err(NeuralError::ProcessTimeout {
                timeout_ms: 180_000,
                ..
            })
        ));
    }
}
