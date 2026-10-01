use crate::error::{NeuralError, Result};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_REQUEST_WARMUP_TIMEOUT_MS: u64 = 2_000;

pub(crate) struct DeferredEngine<T, F>
where
    T: Send + Sync + 'static,
    F: Fn() -> Result<T> + Send + Sync,
{
    engine_id: String,
    factory: Arc<F>,
    request_warmup_timeout_ms: u64,
    state: Arc<Mutex<DeferredEngineState<T>>>,
    condvar: Arc<Condvar>,
}

enum DeferredEngineState<T> {
    NotReady,
    Initializing,
    Failed(String),
    Ready(Arc<T>),
}

impl<T, F> DeferredEngine<T, F>
where
    T: Send + Sync + 'static,
    F: Fn() -> Result<T> + Send + Sync + 'static,
{
    pub(crate) fn new(engine_id: String, factory: F) -> Self {
        Self {
            engine_id,
            factory: Arc::new(factory),
            request_warmup_timeout_ms: DEFAULT_REQUEST_WARMUP_TIMEOUT_MS,
            state: Arc::new(Mutex::new(DeferredEngineState::NotReady)),
            condvar: Arc::new(Condvar::new()),
        }
    }

    pub(crate) fn with_warmup_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.request_warmup_timeout_ms = timeout_ms;
        self
    }

    pub(crate) fn warmup(&self) -> Result<()> {
        self.get(None).map(|_| ())
    }

    pub(crate) fn get_ready_for_request(&self) -> Result<Arc<T>> {
        let timeout = Duration::from_millis(self.request_warmup_timeout_ms);
        self.get(Some(timeout))
    }

    fn get(&self, timeout: Option<Duration>) -> Result<Arc<T>> {
        let mut state = self.state.lock().map_err(|_| NeuralError::ProviderFailed {
            provider_id: self.engine_id.clone(),
            message: "failed to lock native engine state".to_string(),
        })?;

        let deadline = timeout.map(|value| Instant::now() + value);
        loop {
            let action = match &*state {
                DeferredEngineState::Ready(engine) => {
                    return Ok(Arc::clone(engine));
                }
                DeferredEngineState::Failed(error) => {
                    let current_error = error.clone();
                    *state = DeferredEngineState::NotReady;
                    return Err(NeuralError::ProviderFailed {
                        provider_id: self.engine_id.clone(),
                        message: current_error,
                    });
                }
                DeferredEngineState::Initializing => EngineGetAction::Wait,
                DeferredEngineState::NotReady => EngineGetAction::Init,
            };

            match action {
                EngineGetAction::Wait => {
                    if let Some(deadline) = deadline {
                        let now = Instant::now();
                        if now >= deadline {
                            return Err(NeuralError::WarmingUp {
                                engine_id: self.engine_id.clone(),
                                waited_ms: self.request_warmup_timeout_ms,
                            });
                        }
                        let remaining = deadline.saturating_duration_since(now);
                        let wait_result =
                            self.condvar.wait_timeout(state, remaining).map_err(|_| {
                                NeuralError::ProviderFailed {
                                    provider_id: self.engine_id.clone(),
                                    message: "failed to wait for native engine readiness"
                                        .to_string(),
                                }
                            })?;
                        state = wait_result.0;
                        if wait_result.1.timed_out() {
                            return Err(NeuralError::WarmingUp {
                                engine_id: self.engine_id.clone(),
                                waited_ms: self.request_warmup_timeout_ms,
                            });
                        }
                        continue;
                    }
                    state = self
                        .condvar
                        .wait(state)
                        .map_err(|_| NeuralError::ProviderFailed {
                            provider_id: self.engine_id.clone(),
                            message: "failed to wait for native engine readiness".to_string(),
                        })?;
                }
                EngineGetAction::Init => {
                    if timeout.is_none() {
                        *state = DeferredEngineState::Initializing;
                        drop(state);
                        let engine = self.initialize_once();
                        let mut state =
                            self.state.lock().map_err(|_| NeuralError::ProviderFailed {
                                provider_id: self.engine_id.clone(),
                                message: "failed to restore native engine state after init"
                                    .to_string(),
                            })?;
                        match engine {
                            Ok(engine) => {
                                let ready = Arc::new(engine);
                                *state = DeferredEngineState::Ready(Arc::clone(&ready));
                                self.condvar.notify_all();
                                return Ok(ready);
                            }
                            Err(error) => {
                                *state = DeferredEngineState::NotReady;
                                self.condvar.notify_all();
                                return Err(error);
                            }
                        }
                    }

                    *state = DeferredEngineState::Initializing;
                    let background_state = Arc::clone(&self.state);
                    let condvar = Arc::clone(&self.condvar);
                    let factory = Arc::clone(&self.factory);

                    std::thread::spawn(move || {
                        let engine = (factory)();

                        let mut state = match background_state.lock() {
                            Ok(guard) => guard,
                            Err(_) => return,
                        };

                        match engine {
                            Ok(engine) => {
                                *state = DeferredEngineState::Ready(Arc::new(engine));
                            }
                            Err(error) => {
                                *state = DeferredEngineState::Failed(error.to_string());
                            }
                        }
                        condvar.notify_all();
                    });

                    if let Some(deadline) = deadline {
                        let now = Instant::now();
                        if now >= deadline {
                            return Err(NeuralError::WarmingUp {
                                engine_id: self.engine_id.clone(),
                                waited_ms: self.request_warmup_timeout_ms,
                            });
                        }
                        let remaining = deadline.saturating_duration_since(now);
                        let wait_result =
                            self.condvar.wait_timeout(state, remaining).map_err(|_| {
                                NeuralError::ProviderFailed {
                                    provider_id: self.engine_id.clone(),
                                    message: "failed to wait for native engine readiness"
                                        .to_string(),
                                }
                            })?;
                        state = wait_result.0;
                        if wait_result.1.timed_out() {
                            return Err(NeuralError::WarmingUp {
                                engine_id: self.engine_id.clone(),
                                waited_ms: self.request_warmup_timeout_ms,
                            });
                        }
                        continue;
                    }

                    state = self
                        .condvar
                        .wait(state)
                        .map_err(|_| NeuralError::ProviderFailed {
                            provider_id: self.engine_id.clone(),
                            message: "failed to wait for native engine readiness".to_string(),
                        })?;
                }
            }
        }
    }

    fn initialize_once(&self) -> Result<T> {
        (self.factory)()
    }
}

enum EngineGetAction {
    Wait,
    Init,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[derive(Debug)]
    struct FakeEngine {
        marker: u32,
    }

    #[test]
    fn deferred_engine_builds_once_and_reuses_result() {
        let counter = Arc::new(AtomicUsize::new(0));
        let factory_counter = Arc::clone(&counter);
        let engine = DeferredEngine::new("fake".to_string(), move || {
            factory_counter.fetch_add(1, Ordering::SeqCst);
            Ok(FakeEngine { marker: 7 })
        })
        .with_warmup_timeout_ms(200);

        let first = engine.get_ready_for_request().unwrap();
        let second = engine.get_ready_for_request().unwrap();

        assert_eq!(first.marker, 7);
        assert_eq!(second.marker, 7);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn deferred_engine_retries_after_failure_without_caching() {
        let attempt = Arc::new(AtomicUsize::new(0));
        let attempt_tracker = Arc::clone(&attempt);
        let engine = DeferredEngine::new("fake".to_string(), move || {
            let value = attempt_tracker.fetch_add(1, Ordering::SeqCst);
            if value == 0 {
                return Err(NeuralError::ProviderFailed {
                    provider_id: "fake".to_string(),
                    message: "first attempt fails".to_string(),
                });
            }
            Ok(FakeEngine { marker: 9 })
        })
        .with_warmup_timeout_ms(200);

        assert!(engine.get_ready_for_request().is_err());
        let second = engine.get_ready_for_request().unwrap();

        assert_eq!(second.marker, 9);
        assert_eq!(attempt.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn deferred_engine_wait_times_out_with_warming_up_signal() {
        let engine = DeferredEngine::new("fake".to_string(), || {
            std::thread::sleep(Duration::from_millis(200));
            Ok(FakeEngine { marker: 11 })
        })
        .with_warmup_timeout_ms(50);

        let result = engine.get_ready_for_request().err();

        assert!(result.is_some());
        let error = result.unwrap();
        let message = error.to_string();
        assert!(message.contains("still warming up"));
        assert!(message.contains("fake"));
    }
}
