use crate::{LumviseMcpServer, Result};
use std::io::{BufRead, Write};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Mutex, MutexGuard};
use std::thread;

const STDIO_WORKER_COUNT: usize = 4;
const STDIO_QUEUE_CAPACITY: usize = 64;
const CONTROL_WORKER_COUNT: usize = 2;
const CONTROL_QUEUE_CAPACITY: usize = 16;
const CANCELLATION_QUEUE_CAPACITY: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StdioTrafficClass {
    Invocation,
    Control,
    Cancellation,
}

trait StdioRequestProcessor: Sync {
    fn close_transport(&self) {}
    fn process_line(&self, line: &str) -> Result<Option<String>>;

    fn classify_line(&self, _line: &str) -> StdioTrafficClass {
        StdioTrafficClass::Invocation
    }

    fn overload_response(&self, _line: &str) -> Option<String> {
        None
    }

    fn prepare_line(&self, _line: &str) -> Result<()> {
        Ok(())
    }

    fn reject_line(&self, _line: &str) {}
}

impl StdioRequestProcessor for LumviseMcpServer {
    fn close_transport(&self) {
        self.close_transport();
    }
    fn process_line(&self, line: &str) -> Result<Option<String>> {
        self.handle_json_line(line)
    }

    fn classify_line(&self, line: &str) -> StdioTrafficClass {
        self.classify_json_line(line)
    }

    fn overload_response(&self, line: &str) -> Option<String> {
        self.overload_json_line(line)
    }

    fn prepare_line(&self, line: &str) -> Result<()> {
        self.prepare_json_line(line)
    }

    fn reject_line(&self, line: &str) {
        self.reject_json_line(line);
    }
}

/// Runs a Lumvise MCP server over newline-delimited stdio streams.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use lumvise_mcp_core::{McpApplication, McpApplicationError, McpTool};
/// use serde_json::Value;
/// struct EmptyApplication;
/// impl McpApplication for EmptyApplication {
///     fn list_tools(&self) -> Result<Vec<McpTool>, McpApplicationError> { Ok(Vec::new()) }
///     fn invoke_tool(&self, name: &str, _: Value) -> Result<Value, McpApplicationError> {
///         Err(McpApplicationError::invalid_params(format!("unknown tool {name:?}")))
///     }
/// }
/// let server = lumvise_mcp_core::LumviseMcpServer::new(Arc::new(EmptyApplication));
/// let input = std::io::Cursor::new(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
/// let mut output = Vec::new();
/// lumvise_mcp_core::run_stdio(server, input, &mut output).unwrap();
/// assert!(!output.is_empty());
/// ```
pub fn run_stdio<R, W>(server: LumviseMcpServer, reader: R, writer: W) -> Result<()>
where
    R: BufRead,
    W: Write + Send,
{
    run_stdio_requests(&server, reader, writer)
}

fn run_stdio_requests<P, R, W>(processor: &P, reader: R, writer: W) -> Result<()>
where
    P: StdioRequestProcessor,
    R: BufRead,
    W: Write + Send,
{
    let writer = Mutex::new(writer);
    let (invocation_sender, invocation_receiver) = sync_channel(STDIO_QUEUE_CAPACITY);
    let (control_sender, control_receiver) = sync_channel(CONTROL_QUEUE_CAPACITY);
    let (cancellation_sender, cancellation_receiver) = sync_channel(CANCELLATION_QUEUE_CAPACITY);
    let invocation_receiver = Mutex::new(invocation_receiver);
    let control_receiver = Mutex::new(control_receiver);
    let cancellation_receiver = Mutex::new(cancellation_receiver);
    thread::scope(|scope| {
        let mut workers = Vec::with_capacity(STDIO_WORKER_COUNT + CONTROL_WORKER_COUNT + 1);
        for _ in 0..STDIO_WORKER_COUNT {
            workers
                .push(scope.spawn(|| process_requests(processor, &invocation_receiver, &writer)));
        }
        for _ in 0..CONTROL_WORKER_COUNT {
            workers.push(scope.spawn(|| process_requests(processor, &control_receiver, &writer)));
        }
        workers.push(scope.spawn(|| process_requests(processor, &cancellation_receiver, &writer)));
        let dispatch_result = dispatch_requests(
            processor,
            reader,
            &writer,
            invocation_sender,
            control_sender,
            cancellation_sender,
        );
        processor.close_transport();
        let worker_result = join_workers(workers);
        dispatch_result?;
        worker_result
    })
}

fn dispatch_requests<P, R, W>(
    processor: &P,
    reader: R,
    writer: &Mutex<W>,
    invocation_sender: SyncSender<String>,
    control_sender: SyncSender<String>,
    cancellation_sender: SyncSender<String>,
) -> Result<()>
where
    P: StdioRequestProcessor,
    R: BufRead,
    W: Write,
{
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        processor.prepare_line(&line)?;
        let sender = match processor.classify_line(&line) {
            StdioTrafficClass::Invocation => &invocation_sender,
            StdioTrafficClass::Control => &control_sender,
            StdioTrafficClass::Cancellation => &cancellation_sender,
        };
        if let Err(error) = sender.try_send(line) {
            handle_dispatch_rejection(processor, writer, error)?;
        }
    }
    Ok(())
}

fn handle_dispatch_rejection<P, W>(
    processor: &P,
    writer: &Mutex<W>,
    error: TrySendError<String>,
) -> Result<()>
where
    P: StdioRequestProcessor,
    W: Write,
{
    match error {
        TrySendError::Full(line) => {
            processor.reject_line(&line);
            if let Some(response) = processor.overload_response(&line) {
                write_serialized_response(writer, &response)?;
            } else {
                write_response(processor, writer, &line)?;
            }
            Ok(())
        }
        TrySendError::Disconnected(line) => {
            processor.reject_line(&line);
            Err(queue_closed_error(line))
        }
    }
}

fn process_requests<P, W>(
    processor: &P,
    receiver: &Mutex<Receiver<String>>,
    writer: &Mutex<W>,
) -> Result<()>
where
    P: StdioRequestProcessor,
    W: Write,
{
    while let Some(line) = receive_request(receiver)? {
        if let Err(error) = write_response(processor, writer, &line) {
            processor.close_transport();
            return Err(error);
        }
    }
    Ok(())
}

fn receive_request(receiver: &Mutex<Receiver<String>>) -> Result<Option<String>> {
    let receiver = lock_or_io_error(receiver, "stdio request receiver")?;
    Ok(receiver.recv().ok())
}

fn write_response<P, W>(processor: &P, writer: &Mutex<W>, line: &str) -> Result<()>
where
    P: StdioRequestProcessor,
    W: Write,
{
    if let Some(response) = processor.process_line(line)? {
        write_serialized_response(writer, &response)?;
    }
    Ok(())
}

fn write_serialized_response<W>(writer: &Mutex<W>, response: &str) -> Result<()>
where
    W: Write,
{
    let mut writer = lock_or_io_error(writer, "stdio response writer")?;
    writer.write_all(response.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn join_workers<T>(workers: Vec<thread::ScopedJoinHandle<'_, Result<T>>>) -> Result<()> {
    for worker in workers {
        worker.join().map_err(worker_panic_error)??;
    }
    Ok(())
}

fn lock_or_io_error<'value, T>(
    mutex: &'value Mutex<T>,
    name: &str,
) -> Result<MutexGuard<'value, T>> {
    mutex.lock().map_err(|_| {
        std::io::Error::other(format!(
            "poisoned mutex {name:?}; expected an available lock"
        ))
        .into()
    })
}

fn queue_closed_error(line: String) -> crate::McpCoreError {
    std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        format!(
            "closed stdio request queue for line {:?}; expected an active worker",
            line
        ),
    )
    .into()
}

fn worker_panic_error(_: Box<dyn std::any::Any + Send>) -> crate::McpCoreError {
    std::io::Error::other("stdio request worker panicked; expected a completed response").into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        McpApplication, McpApplicationError, McpInvocationContext, McpInvocationFailureKind,
        McpTool,
    };
    use std::io::Cursor;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    struct CoordinatedFakeProcessor {
        second_started: (Mutex<bool>, Condvar),
        first_observed_second: AtomicBool,
    }

    impl CoordinatedFakeProcessor {
        fn new() -> Self {
            Self {
                second_started: (Mutex::new(false), Condvar::new()),
                first_observed_second: AtomicBool::new(false),
            }
        }
    }

    impl StdioRequestProcessor for CoordinatedFakeProcessor {
        fn process_line(&self, line: &str) -> Result<Option<String>> {
            if line == "second" {
                let (started, ready) = &self.second_started;
                *started.lock().unwrap() = true;
                ready.notify_all();
                return Ok(Some(line.to_string()));
            }

            let (started, ready) = &self.second_started;
            let started = started.lock().unwrap();
            let (started, _) = ready
                .wait_timeout_while(started, Duration::from_millis(200), |value| !*value)
                .unwrap();
            self.first_observed_second.store(*started, Ordering::SeqCst);
            Ok(Some(line.to_string()))
        }
    }

    #[test]
    fn stdio_requests_start_concurrently() {
        let processor = CoordinatedFakeProcessor::new();
        let input = Cursor::new("first\nsecond\n");
        let mut output = Vec::new();

        run_stdio_requests(&processor, input, &mut output).unwrap();

        assert!(processor.first_observed_second.load(Ordering::SeqCst));
    }

    struct SaturatedApplication {
        active: AtomicUsize,
    }

    impl McpApplication for SaturatedApplication {
        fn list_tools(&self) -> std::result::Result<Vec<McpTool>, McpApplicationError> {
            Ok(vec![McpTool::new("quick", "quick", serde_json::json!({}))])
        }

        fn invoke_tool(
            &self,
            _name: &str,
            _arguments: serde_json::Value,
        ) -> std::result::Result<serde_json::Value, McpApplicationError> {
            unreachable!("controlled invocation is required")
        }

        fn invoke_tool_controlled(
            &self,
            context: &McpInvocationContext,
            name: &str,
            _arguments: serde_json::Value,
        ) -> std::result::Result<serde_json::Value, McpApplicationError> {
            if name == "quick" {
                return Ok(serde_json::json!({"completed": true}));
            }
            self.active.fetch_add(1, Ordering::SeqCst);
            while !context.cancellation().is_cancelled() {
                thread::yield_now();
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
            Err(McpApplicationError::controlled_invocation(
                McpInvocationFailureKind::Cancelled,
                "cancelled by stdio regression",
                false,
            ))
        }
    }

    #[test]
    fn control_and_cancellation_progress_while_all_invocation_workers_are_blocked() {
        let application = Arc::new(SaturatedApplication {
            active: AtomicUsize::new(0),
        });
        let server =
            LumviseMcpServer::with_identity(application.clone(), "stdio-owner", "stdio-session");
        let input = Cursor::new(saturated_stdio_input());
        let mut output = Vec::new();

        run_stdio(server, input, &mut output).unwrap();

        assert_eq!(application.active.load(Ordering::SeqCst), 0);
        let responses = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(responses.len(), 8);
        assert!(
            responses
                .iter()
                .any(|value| value["error"]["code"] == -32700)
        );
        assert!(responses.iter().any(|value| value["id"] == "list"));
        assert!(responses.iter().any(|value| value["id"] == "quick"));
        for id in 1..=4 {
            assert!(responses.iter().any(|value| {
                value["id"] == id && value["error"]["data"]["kind"] == "cancelled"
            }));
        }
    }

    fn saturated_stdio_input() -> String {
        let mut lines = vec!["{malformed".to_owned()];
        for id in 1..=4 {
            lines.push(format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"blocked","arguments":{{}}}}}}"#
            ));
        }
        lines.push(r#"{"jsonrpc":"2.0","id":"list","method":"tools/list"}"#.into());
        for id in 1..=4 {
            lines.push(format!(
                r#"{{"jsonrpc":"2.0","method":"notifications/cancelled","params":{{"requestId":{id}}}}}"#
            ));
        }
        lines.push(r#"{"jsonrpc":"2.0","id":"init","method":"initialize"}"#.into());
        lines.push(r#"{"jsonrpc":"2.0","id":"quick","method":"tools/call","params":{"name":"quick","arguments":{}}}"#.into());
        lines.join("\n")
    }
}
