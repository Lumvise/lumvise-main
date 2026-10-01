//! Bounded, isolating local speech scheduler shared by desktop and server.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    time::Duration,
};

use lumvise_resource_routing::InvocationControl;
use thiserror::Error;

use crate::{
    Result as NeuralResult,
    speech::{SpeechRecognizer, SpeechSynthesizer},
    text2voice::{Text2VoiceRequest, Text2VoiceResponse, Text2VoiceStreamEvent},
    voice2text::{Voice2TextRequest, Voice2TextResponse},
};

type ExecutionResult<T> = std::result::Result<T, SpeechExecutionError>;

pub const SPEECH_QUEUE_CAPACITY: usize = 8;
const SPEECH_POLL: Duration = Duration::from_millis(10);

#[derive(Debug, Error)]
pub enum SpeechExecutionError {
    #[error("speech service is unavailable")]
    Unavailable,
    #[error("speech service queue is full")]
    Busy,
    #[error("speech work was cancelled")]
    Cancelled,
    #[error("speech work deadline elapsed")]
    DeadlineExceeded,
    #[error("speech service failed: {0}")]
    Service(String),
}

pub struct SpeechToTextExecutor {
    sender: SyncSender<SpeechToTextJob>,
    isolated: Arc<AtomicBool>,
}

struct SpeechToTextJob {
    request: Voice2TextRequest,
    control: InvocationControl,
    started: Arc<AtomicBool>,
    response: SyncSender<NeuralResult<Voice2TextResponse>>,
}

impl SpeechToTextExecutor {
    pub fn start(service: Arc<dyn SpeechRecognizer>) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<SpeechToTextJob>(SPEECH_QUEUE_CAPACITY);
        let isolated = Arc::new(AtomicBool::new(false));
        let worker_isolated = Arc::clone(&isolated);
        std::thread::spawn(move || {
            while let Ok(job) = receiver.recv() {
                if terminal(&job.control).is_some() {
                    continue;
                }
                job.started.store(true, Ordering::Release);
                let result = service.transcribe(&job.request, &job.control);
                finish(&job.control, job.response, result, &worker_isolated);
            }
        });
        Self { sender, isolated }
    }

    pub fn transcribe(
        &self,
        request: Voice2TextRequest,
        control: InvocationControl,
    ) -> ExecutionResult<Voice2TextResponse> {
        let (response, receiver) = mpsc::sync_channel(1);
        let started = Arc::new(AtomicBool::new(false));
        send(
            &self.sender,
            SpeechToTextJob {
                request,
                control: control.clone(),
                started: Arc::clone(&started),
                response,
            },
            &self.isolated,
        )?;
        wait(receiver, control, started, &self.isolated)
    }
}

pub struct TextToSpeechExecutor {
    sender: SyncSender<TextToSpeechWork>,
    isolated: Arc<AtomicBool>,
}

struct TextToSpeechJob {
    request: Text2VoiceRequest,
    control: InvocationControl,
    started: Arc<AtomicBool>,
    response: SyncSender<NeuralResult<Text2VoiceResponse>>,
}

struct TextToSpeechStreamJob {
    request: Text2VoiceRequest,
    control: InvocationControl,
    on_event: Box<dyn FnMut(Text2VoiceStreamEvent) -> NeuralResult<()> + Send>,
}

enum TextToSpeechWork {
    Synthesize(TextToSpeechJob),
    Stream(TextToSpeechStreamJob),
}

impl TextToSpeechExecutor {
    pub fn start(service: Arc<dyn SpeechSynthesizer>) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<TextToSpeechWork>(SPEECH_QUEUE_CAPACITY);
        let isolated = Arc::new(AtomicBool::new(false));
        let worker_isolated = Arc::clone(&isolated);
        std::thread::spawn(move || {
            while let Ok(work) = receiver.recv() {
                match work {
                    TextToSpeechWork::Synthesize(job) => {
                        if terminal(&job.control).is_some() {
                            continue;
                        }
                        job.started.store(true, Ordering::Release);
                        let result = service.synthesize(&job.request, &job.control);
                        finish(&job.control, job.response, result, &worker_isolated);
                    }
                    TextToSpeechWork::Stream(job) => {
                        if terminal(&job.control).is_some() {
                            continue;
                        }
                        run_stream_job(service.as_ref(), job);
                    }
                }
            }
        });
        Self { sender, isolated }
    }

    pub fn synthesize(
        &self,
        request: Text2VoiceRequest,
        control: InvocationControl,
    ) -> ExecutionResult<Text2VoiceResponse> {
        let (response, receiver) = mpsc::sync_channel(1);
        let started = Arc::new(AtomicBool::new(false));
        send(
            &self.sender,
            TextToSpeechWork::Synthesize(TextToSpeechJob {
                request,
                control: control.clone(),
                started: Arc::clone(&started),
                response,
            }),
            &self.isolated,
        )?;
        wait(receiver, control, started, &self.isolated)
    }

    /// Enqueues text for background streaming synthesis on the same bounded,
    /// isolating worker used by [`synthesize`](Self::synthesize) and returns
    /// as soon as the job is accepted — the caller never blocks on synthesis
    /// completing. `on_event` runs on the worker thread as chunks are
    /// produced, so it must be cheap and `Send + 'static` (typically a
    /// closure that forwards into a bounded transport channel).
    ///
    /// `request.text` is markdown-sanitized and split into sentence-sized
    /// chunks (see [`chunk_for_speech`]) before synthesis, so one long
    /// request and many short ones stream identically (time-to-first-audio
    /// after roughly one sentence either way). `sequence` on every emitted
    /// [`Text2VoiceStreamEvent::AudioChunk`] is monotonic across the whole
    /// request; exactly one [`Text2VoiceStreamEvent::Complete`] is emitted
    /// at the end, or one [`Text2VoiceStreamEvent::Error`] on failure.
    pub fn enqueue_stream<F>(
        &self,
        request: Text2VoiceRequest,
        control: InvocationControl,
        on_event: F,
    ) -> ExecutionResult<()>
    where
        F: FnMut(Text2VoiceStreamEvent) -> NeuralResult<()> + Send + 'static,
    {
        send(
            &self.sender,
            TextToSpeechWork::Stream(TextToSpeechStreamJob {
                request,
                control,
                on_event: Box::new(on_event),
            }),
            &self.isolated,
        )
    }
}

fn run_stream_job(service: &dyn SpeechSynthesizer, job: TextToSpeechStreamJob) {
    let TextToSpeechStreamJob {
        request,
        control,
        mut on_event,
    } = job;
    let chunks = chunk_for_speech(&sanitize_for_speech(&request.text));
    let mut next_sequence: u64 = 0;
    let mut failed = false;
    for chunk_text in chunks {
        if terminal(&control).is_some() {
            failed = true;
            break;
        }
        let chunk_request = Text2VoiceRequest {
            text: chunk_text,
            ..request.clone()
        };
        let result = service.stream_with_events(&chunk_request, &control, &mut |event| {
            match event {
                Text2VoiceStreamEvent::AudioChunk {
                    audio, media_type, ..
                } => {
                    let sequence = next_sequence;
                    next_sequence += 1;
                    on_event(Text2VoiceStreamEvent::AudioChunk {
                        sequence,
                        audio,
                        media_type,
                    })
                }
                // One sentence-chunk's own `Complete` is not the whole
                // request's `Complete` — swallow it; we emit exactly one
                // after the last chunk below.
                Text2VoiceStreamEvent::Complete => Ok(()),
                Text2VoiceStreamEvent::Error { message } => {
                    failed = true;
                    on_event(Text2VoiceStreamEvent::Error { message })
                }
            }
        });
        if result.is_err() || failed {
            failed = true;
            break;
        }
    }
    if !failed {
        let _ = on_event(Text2VoiceStreamEvent::Complete);
    }
}

/// Minimum/maximum characters per streamed speech chunk (D3: response
/// granularity must not matter for time-to-first-audio).
pub const SPEECH_CHUNK_MIN_CHARS: usize = 40;
pub const SPEECH_CHUNK_MAX_CHARS: usize = 250;

/// Strips markdown emphasis/heading/list/link/code markup that a listener
/// must never hear spoken aloud ("asterisk", "backtick", ...). Runs before
/// [`chunk_for_speech`] so sentence boundaries are computed on the text a
/// listener actually hears.
pub fn sanitize_for_speech(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' | '_' | '`' | '#' => {}
            '[' => {}
            ']' => {
                if chars.peek() == Some(&'(') {
                    for next in chars.by_ref() {
                        if next == ')' {
                            break;
                        }
                    }
                }
            }
            _ => stripped.push(ch),
        }
    }
    stripped
        .lines()
        .map(|line| line.trim_start().trim_start_matches(['-', '•', '>']).trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Splits sanitized text into speech-sized sentence chunks
/// (`SPEECH_CHUNK_MIN_CHARS..=SPEECH_CHUNK_MAX_CHARS` characters), flushing
/// a short tail rather than dropping it.
pub fn chunk_for_speech(text: &str) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for sentence in split_sentences(text) {
        if !current.is_empty() && current.len() + 1 + sentence.len() > SPEECH_CHUNK_MAX_CHARS {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(&sentence);
        if current.len() >= SPEECH_CHUNK_MIN_CHARS {
            chunks.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        current.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            let mid_word = matches!(chars.peek(), Some(next) if next.is_alphanumeric());
            if !mid_word {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    sentences.push(trimmed.to_string());
                }
                current.clear();
            }
        }
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        sentences.push(trimmed.to_string());
    }
    sentences
}

fn send<T>(sender: &SyncSender<T>, job: T, isolated: &AtomicBool) -> ExecutionResult<()> {
    if isolated.load(Ordering::Acquire) {
        return Err(SpeechExecutionError::Unavailable);
    }
    sender.try_send(job).map_err(|error| match error {
        TrySendError::Full(_) => SpeechExecutionError::Busy,
        TrySendError::Disconnected(_) => SpeechExecutionError::Unavailable,
    })
}

fn wait<T>(
    receiver: mpsc::Receiver<NeuralResult<T>>,
    control: InvocationControl,
    started: Arc<AtomicBool>,
    isolated: &AtomicBool,
) -> ExecutionResult<T> {
    loop {
        if let Some(error) = terminal(&control) {
            if started.load(Ordering::Acquire) {
                isolated.store(true, Ordering::Release);
            }
            return Err(error);
        }
        match receiver.recv_timeout(SPEECH_POLL) {
            Ok(Ok(response)) => return Ok(response),
            Ok(Err(error)) => return Err(SpeechExecutionError::Service(error.to_string())),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(SpeechExecutionError::Unavailable);
            }
        }
    }
}

fn finish<T>(
    control: &InvocationControl,
    response: SyncSender<NeuralResult<T>>,
    result: NeuralResult<T>,
    isolated: &AtomicBool,
) {
    if terminal(control).is_none() {
        let _ = response.send(result);
    } else {
        isolated.store(true, Ordering::Release);
    }
}

fn terminal(control: &InvocationControl) -> Option<SpeechExecutionError> {
    if control.is_cancelled() {
        Some(SpeechExecutionError::Cancelled)
    } else if control.is_expired() {
        Some(SpeechExecutionError::DeadlineExceeded)
    } else {
        None
    }
}
