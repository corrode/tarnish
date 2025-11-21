//! # Tarnish - Process Isolation Library
//!
//! Tarnish provides process-level isolation for running Rust code with automatic
//! panic recovery and graceful shutdown. Implement the [`Worker`] trait and let
//! [`Process`] handle the lifecycle management.
//!
//! # Features
//!
//! - **Trait-based API**: Implement [`Worker`] trait with your business logic
//! - **Auto-restart on panic**: Child processes automatically restart if they panic
//! - **Graceful shutdown**: Automatic cleanup when [`Process`] is dropped
//! - **Type-safe**: Generic over your worker type
//! - **Zero dependencies**: Built entirely on Rust standard library
//!
//! # Example
//!
//! ```no_run
//! use tarnish::{Worker, Process, worker_main};
//! use std::fmt;
//!
//! // Define your worker
//! #[derive(Default)]
//! struct Calculator;
//!
//! #[derive(Debug)]
//! struct CalcError(String);
//!
//! impl fmt::Display for CalcError {
//!     fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
//!         write!(f, "{}", self.0)
//!     }
//! }
//!
//! impl Worker for Calculator {
//!     type Error = CalcError;
//!
//!     fn run(&mut self, input: &str) -> Result<String, Self::Error> {
//!         let num: i32 = input.parse()
//!             .map_err(|e| CalcError(format!("Parse error: {}", e)))?;
//!         Ok(format!("Result: {}", num * 2))
//!     }
//! }
//!
//! fn main() {
//!     // Handle child process mode
//!     if let Some(exit_code) = worker_main::<Calculator>() {
//!         std::process::exit(exit_code);
//!     }
//!
//!     // Parent process - spawn and use the worker
//!     let mut process = Process::<Calculator>::spawn()
//!         .expect("Failed to spawn process");
//!
//!     match process.call("42") {
//!         Ok(result) => println!("Success: {}", result),
//!         Err(e) => eprintln!("Error: {}", e),
//!     }
//! }
//! ```

use std::any;
use std::env;
use std::fmt;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Environment variable prefix for child process detection
/// The full variable name is: __TARNISH_WORKER_{TypeName}__
const WORKER_ENV_PREFIX: &str = "__TARNISH_WORKER_";

/// Maximum time to wait for graceful shutdown before sending SIGKILL
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Generate a unique environment variable name for a worker type
fn worker_env_name<T: 'static>() -> String {
    let type_name = any::type_name::<T>()
        .replace("::", "_")
        .replace("<", "_")
        .replace(">", "_");
    format!("{}{}__", WORKER_ENV_PREFIX, type_name)
}

/// Protocol message types for parent-child communication
#[derive(Debug)]
enum Message {
    /// Request from parent to child
    Request(String),
    /// Successful response from child to parent
    Response(String),
    /// Error response from child to parent
    Error(String),
    /// Shutdown signal from parent to child
    Shutdown,
    /// Ping to check if child is alive
    Ping,
    /// Pong response to ping
    Pong,
}

impl Message {
    fn encode(&self) -> String {
        match self {
            Message::Request(s) => format!("REQ:{}", s),
            Message::Response(s) => format!("RES:{}", s),
            Message::Error(s) => format!("ERR:{}", s),
            Message::Shutdown => "SHUTDOWN".to_string(),
            Message::Ping => "PING".to_string(),
            Message::Pong => "PONG".to_string(),
        }
    }

    fn decode(s: &str) -> std::result::Result<Self, String> {
        if s == "SHUTDOWN" {
            return Ok(Message::Shutdown);
        }
        if s == "PING" {
            return Ok(Message::Ping);
        }
        if s == "PONG" {
            return Ok(Message::Pong);
        }

        if let Some(payload) = s.strip_prefix("REQ:") {
            return Ok(Message::Request(payload.to_string()));
        }
        if let Some(payload) = s.strip_prefix("RES:") {
            return Ok(Message::Response(payload.to_string()));
        }
        if let Some(payload) = s.strip_prefix("ERR:") {
            return Ok(Message::Error(payload.to_string()));
        }

        Err(format!("Invalid message format: {}", s))
    }
}

/// Error types for Process operations
#[derive(Debug)]
pub enum ProcessError {
    /// Failed to spawn the child process
    SpawnError(io::Error),
    /// Failed to get the current executable path
    ExecutablePathError(io::Error),
    /// Failed to communicate with the child process
    CommunicationError(io::Error),
    /// Child process terminated unexpectedly
    ProcessTerminated,
    /// Child process panicked
    ProcessPanicked(String),
    /// Worker error from child
    WorkerError(String),
    /// Protocol error
    ProtocolError(String),
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProcessError::SpawnError(e) => write!(f, "Failed to spawn process: {}", e),
            ProcessError::ExecutablePathError(e) => write!(f, "Failed to get executable path: {}", e),
            ProcessError::CommunicationError(e) => write!(f, "Communication error: {}", e),
            ProcessError::ProcessTerminated => write!(f, "Process terminated unexpectedly"),
            ProcessError::ProcessPanicked(msg) => write!(f, "Process panicked: {}", msg),
            ProcessError::WorkerError(msg) => write!(f, "Worker error: {}", msg),
            ProcessError::ProtocolError(msg) => write!(f, "Protocol error: {}", msg),
        }
    }
}

impl std::error::Error for ProcessError {}

impl From<io::Error> for ProcessError {
    fn from(err: io::Error) -> Self {
        ProcessError::CommunicationError(err)
    }
}

pub type Result<T> = std::result::Result<T, ProcessError>;

/// Trait for encoding messages to send over process boundaries
///
/// # Implementation
///
/// When the `serde` feature is enabled (default), this is automatically implemented
/// for all types that implement `serde::Serialize` using postcard serialization.
///
/// **Note**: The serialization format (currently postcard) is an implementation detail
/// and may change in future versions for performance or compatibility improvements.
///
/// Without the `serde` feature, implement this manually for your types.
pub trait MessageEncode {
    /// Encode the message to a string for transmission
    fn encode(&self) -> String;
}

/// Trait for decoding messages received over process boundaries
///
/// # Implementation
///
/// When the `serde` feature is enabled (default), this is automatically implemented
/// for all types that implement `serde::Deserialize` using postcard deserialization.
///
/// **Note**: The serialization format (currently postcard) is an implementation detail
/// and may change in future versions for performance or compatibility improvements.
///
/// Without the `serde` feature, implement this manually for your types.
pub trait MessageDecode: Sized {
    /// Decode a string into a message
    ///
    /// Returns an error if the string cannot be decoded.
    fn decode(s: &str) -> std::result::Result<Self, String>;
}

// Automatic serialization via serde (enabled by default)
#[cfg(feature = "serde")]
mod serde_impl {
    use super::{MessageDecode, MessageEncode};
    use serde::{Deserialize, Serialize};

    /// Blanket implementation for all Serialize types
    ///
    /// Uses postcard for compact binary serialization, then base64 encoding
    /// for safe string transmission over stdin/stdout.
    impl<T: Serialize> MessageEncode for T {
        fn encode(&self) -> String {
            let bytes = postcard::to_allocvec(self)
                .expect("Serialization should not fail for valid types");
            // Use base64 encoding for safe string transmission
            base64_encode(&bytes)
        }
    }

    /// Blanket implementation for all Deserialize types
    impl<T: for<'de> Deserialize<'de>> MessageDecode for T {
        fn decode(s: &str) -> std::result::Result<Self, String> {
            let bytes = base64_decode(s)
                .map_err(|e| format!("Base64 decode error: {}", e))?;
            postcard::from_bytes(&bytes)
                .map_err(|e| format!("Deserialization error: {}", e))
        }
    }

    // Simple base64 encoding/decoding using standard base64 alphabet
    fn base64_encode(bytes: &[u8]) -> String {
        const BASE64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut result = String::new();

        for chunk in bytes.chunks(3) {
            let mut buf = [0u8; 3];
            for (i, &byte) in chunk.iter().enumerate() {
                buf[i] = byte;
            }

            let b1 = (buf[0] >> 2) as usize;
            let b2 = (((buf[0] & 0x03) << 4) | (buf[1] >> 4)) as usize;
            let b3 = (((buf[1] & 0x0F) << 2) | (buf[2] >> 6)) as usize;
            let b4 = (buf[2] & 0x3F) as usize;

            result.push(BASE64_CHARS[b1] as char);
            result.push(BASE64_CHARS[b2] as char);
            result.push(if chunk.len() > 1 { BASE64_CHARS[b3] as char } else { '=' });
            result.push(if chunk.len() > 2 { BASE64_CHARS[b4] as char } else { '=' });
        }

        result
    }

    fn base64_decode(s: &str) -> std::result::Result<Vec<u8>, String> {
        let s = s.trim_end_matches('=');
        let mut result = Vec::new();
        let mut buf = 0u32;
        let mut bits = 0;

        for ch in s.chars() {
            let val = match ch {
                'A'..='Z' => ch as u32 - 'A' as u32,
                'a'..='z' => ch as u32 - 'a' as u32 + 26,
                '0'..='9' => ch as u32 - '0' as u32 + 52,
                '+' => 62,
                '/' => 63,
                _ => return Err(format!("Invalid base64 character: {}", ch)),
            };

            buf = (buf << 6) | val;
            bits += 6;

            if bits >= 8 {
                bits -= 8;
                result.push((buf >> bits) as u8);
                buf &= (1 << bits) - 1;
            }
        }

        Ok(result)
    }
}

// Manual implementation when serde feature is disabled
#[cfg(not(feature = "serde"))]
mod manual_impl {
    use super::{MessageDecode, MessageEncode};

    // Blanket implementations for String when serde is disabled
    impl MessageEncode for String {
        fn encode(&self) -> String {
            self.clone()
        }
    }

    impl MessageDecode for String {
        fn decode(s: &str) -> std::result::Result<Self, String> {
            Ok(s.to_string())
        }
    }

    impl MessageEncode for &str {
        fn encode(&self) -> String {
            self.to_string()
        }
    }
}

/// Trait for implementing worker logic that runs in an isolated process
///
/// Implement this trait to define your business logic with your own message types.
///
/// # Example
///
/// ```
/// use tarnish::Worker;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Default)]
/// struct MyWorker;
///
/// // Define your message types with serde
/// #[derive(Serialize, Deserialize)]
/// enum Request {
///     Add(i32, i32),
///     Multiply(i32, i32),
/// }
///
/// #[derive(Serialize, Deserialize)]
/// enum Response {
///     Result(i32),
/// }
///
/// // Automatic serialization via postcard - no manual encoding needed!
/// impl Worker for MyWorker {
///     type Input = Request;
///     type Output = Response;
///     type Error = String;
///
///     fn run(&mut self, input: Request) -> Result<Response, String> {
///         let result = match input {
///             Request::Add(a, b) => a + b,
///             Request::Multiply(a, b) => a * b,
///         };
///         Ok(Response::Result(result))
///     }
/// }
/// ```
pub trait Worker: Default + 'static {
    /// Input message type (must be encodable for retry and decodable in worker)
    type Input: MessageEncode + MessageDecode;
    /// Output message type (must be encodable in worker and decodable in parent)
    type Output: MessageEncode + MessageDecode;
    /// Error type that can be returned from the worker
    type Error: fmt::Display;

    /// Process a request
    ///
    /// This method is called in the worker process for each request from the parent.
    fn run(&mut self, input: Self::Input) -> std::result::Result<Self::Output, Self::Error>;
}

/// Handle to a worker process
///
/// Manages the lifecycle of a child process running your [`Worker`] implementation.
/// Automatically restarts on panic and performs graceful shutdown when dropped.
///
/// # Example
///
/// ```no_run
/// use tarnish::{Worker, Process};
///
/// #[derive(Default)]
/// struct MyWorker;
///
/// impl Worker for MyWorker {
///     type Error = String;
///     fn run(&mut self, input: &str) -> Result<String, String> {
///         Ok(format!("Processed: {}", input))
///     }
/// }
///
/// let mut process = Process::<MyWorker>::spawn().unwrap();
/// let result = process.call("hello").unwrap();
/// ```
pub struct Process<W: Worker> {
    child: Child,
    stdin: BufWriter<std::process::ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
    _phantom: std::marker::PhantomData<W>,
}

impl<W: Worker> Process<W> {
    /// Spawn a new worker process
    ///
    /// This spawns a new instance of the current binary which should call
    /// `worker_main::<W>()` in its main function.
    pub fn spawn() -> Result<Self> {
        Self::spawn_internal()
    }

    fn spawn_internal() -> Result<Self> {
        let exe_path = env::current_exe().map_err(ProcessError::ExecutablePathError)?;
        let env_name = worker_env_name::<W>();

        let mut child = Command::new(exe_path)
            .env(&env_name, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(ProcessError::SpawnError)?;

        let stdin = child.stdin.take().expect("Failed to get child stdin");
        let stdout = child.stdout.take().expect("Failed to get child stdout");

        Ok(Process {
            child,
            stdin: BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
            _phantom: std::marker::PhantomData,
        })
    }

    /// Call the worker with input and wait for response
    ///
    /// Sends a request to the worker process and waits for a response.
    /// If the worker panics, automatically restarts and retries once.
    pub fn call(&mut self, input: W::Input) -> Result<W::Output> {
        self.call_with_retry(input, true)
    }

    fn call_with_retry(&mut self, input: W::Input, allow_retry: bool) -> Result<W::Output> {
        // Encode once for potential retries
        let encoded_input = input.encode();
        self.call_with_encoded(&encoded_input, allow_retry)
    }

    fn call_with_encoded(&mut self, encoded_input: &str, allow_retry: bool) -> Result<W::Output> {
        // Send request
        if let Err(e) = self.send_message(&Message::Request(encoded_input.to_string())) {
            // Failed to send, child likely dead
            if allow_retry {
                eprintln!("[PARENT] Worker not responding, restarting...");
                self.restart()?;
                return self.call_with_encoded(encoded_input, false);
            } else {
                return Err(e);
            }
        }

        // Receive response
        match self.receive_message() {
            Ok(Message::Response(encoded_output)) => {
                // Decode the output
                W::Output::decode(&encoded_output)
                    .map_err(|e| ProcessError::ProtocolError(format!("Failed to decode output: {}", e)))
            }
            Ok(Message::Error(err)) => Err(ProcessError::WorkerError(err)),
            Ok(msg) => {
                // Unexpected message type
                if allow_retry {
                    eprintln!("[PARENT] Unexpected message, restarting...");
                    self.restart()?;
                    self.call_with_encoded(encoded_input, false)
                } else {
                    Err(ProcessError::ProtocolError(format!(
                        "Unexpected message: {:?}",
                        msg
                    )))
                }
            }
            Err(ProcessError::ProcessTerminated) | Err(ProcessError::CommunicationError(_)) => {
                // Child died or communication failed
                if allow_retry {
                    eprintln!("[PARENT] Worker terminated, restarting...");
                    self.restart()?;
                    self.call_with_encoded(encoded_input, false)
                } else {
                    Err(ProcessError::ProcessTerminated)
                }
            }
            Err(e) => Err(e),
        }
    }

    fn send_message(&mut self, msg: &Message) -> Result<()> {
        let encoded = msg.encode();
        writeln!(self.stdin, "{}", encoded)?;
        self.stdin.flush()?;
        Ok(())
    }

    fn receive_message(&mut self) -> Result<Message> {
        let mut line = String::new();
        let bytes_read = self.stdout.read_line(&mut line)?;

        if bytes_read == 0 {
            return Err(ProcessError::ProcessTerminated);
        }

        // Remove trailing newline
        let line = line.trim_end();

        Message::decode(line).map_err(|e| ProcessError::ProtocolError(e))
    }

    fn restart(&mut self) -> Result<()> {
        // Kill old child
        let _ = self.child.kill();
        let _ = self.child.wait();

        // Spawn new child and replace self with it
        let new_handle = Self::spawn_internal()?;
        *self = new_handle;

        Ok(())
    }

    /// Check if the worker process is still running
    pub fn is_running(&mut self) -> Result<bool> {
        match self.child.try_wait() {
            Ok(Some(_)) => Ok(false),
            Ok(None) => Ok(true),
            Err(e) => Err(ProcessError::CommunicationError(e)),
        }
    }
}

impl<W: Worker> Drop for Process<W> {
    fn drop(&mut self) {
        // Try graceful shutdown first by sending shutdown message
        if self.send_message(&Message::Shutdown).is_ok() {
            let start = Instant::now();

            // Wait for child to exit gracefully
            while start.elapsed() < GRACEFUL_SHUTDOWN_TIMEOUT {
                if let Ok(Some(_)) = self.child.try_wait() {
                    return; // Child exited gracefully
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        // Graceful shutdown timed out or failed, force kill
        // child.kill() sends SIGKILL on Unix and TerminateProcess on Windows
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Handle worker process mode in your main function
///
/// Call this at the start of your main() function. If it returns `Some(exit_code)`,
/// you're running in worker mode and should exit with that code.
///
/// # Example
///
/// ```no_run
/// use tarnish::{Worker, worker_main};
///
/// #[derive(Default)]
/// struct MyWorker;
///
/// impl Worker for MyWorker {
///     type Error = String;
///     fn run(&mut self, input: &str) -> Result<String, String> {
///         Ok(format!("Processed: {}", input))
///     }
/// }
///
/// fn main() {
///     if let Some(exit_code) = worker_main::<MyWorker>() {
///         std::process::exit(exit_code);
///     }
///
///     // Parent process logic here
/// }
/// ```
pub fn worker_main<W: Worker>() -> Option<i32> {
    let env_name = worker_env_name::<W>();
    if env::var(&env_name).is_err() {
        return None; // Not a worker process
    }

    // We're in worker mode
    let exit_code = run_worker_loop::<W>();
    Some(exit_code)
}

fn run_worker_loop<W: Worker>() -> i32 {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdin = BufReader::new(stdin);
    let mut stdout = BufWriter::new(stdout);

    // Create the worker instance
    let mut worker = W::default();

    loop {
        let mut line = String::new();

        // Read message from parent
        let bytes_read = match stdin.read_line(&mut line) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("[CHILD] Failed to read from parent: {}", e);
                return 1;
            }
        };

        if bytes_read == 0 {
            // Parent closed stdin, exit gracefully
            return 0;
        }

        let line = line.trim_end();

        // Decode message
        let message = match Message::decode(line) {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("[CHILD] Protocol error: {}", e);
                return 1;
            }
        };

        match message {
            Message::Shutdown => {
                // Graceful shutdown requested
                return 0;
            }
            Message::Ping => {
                // Health check
                if send_message(&mut stdout, &Message::Pong).is_err() {
                    return 1;
                }
            }
            Message::Request(encoded_input) => {
                // Decode the input
                let input = match W::Input::decode(&encoded_input) {
                    Ok(inp) => inp,
                    Err(e) => {
                        eprintln!("[WORKER] Failed to decode input: {}", e);
                        let err_msg = Message::Error(format!("Decode error: {}", e));
                        if send_message(&mut stdout, &err_msg).is_err() {
                            return 1;
                        }
                        continue;
                    }
                };

                // Run business logic - panics will kill this process
                // and the parent will detect and restart
                let response = match worker.run(input) {
                    Ok(output) => Message::Response(output.encode()),
                    Err(err) => Message::Error(err.to_string()),
                };

                if send_message(&mut stdout, &response).is_err() {
                    return 1;
                }
            }
            _ => {
                eprintln!("[CHILD] Unexpected message type");
                return 1;
            }
        }
    }
}

fn send_message(stdout: &mut BufWriter<io::Stdout>, msg: &Message) -> io::Result<()> {
    writeln!(stdout, "{}", msg.encode())?;
    stdout.flush()?;
    Ok(())
}
