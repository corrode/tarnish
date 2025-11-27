//! # tarnish, a process Isolation Library
//!
//! tarnish provides process-level isolation for running Rust code with automatic
//! panic recovery and graceful shutdown. Implement the [`Task`] trait and let
//! [`Process`] handle the lifecycle management.
//!
//! # Features
//!
//! - **Trait-based API**: Implement [`Task`] trait with your business logic
//! - **Auto-restart on panic**: Child processes automatically restart if they panic
//! - **Graceful shutdown**: Automatic cleanup when [`Process`] is dropped
//! - **Type-safe**: Generic over your worker type
//! - **Zero dependencies**: Built entirely on Rust standard library
//!
//! # Example
//!
//! ```no_run
//! use tarnish::{Task, Process};
//!
//! #[derive(Default)]
//! struct Calculator;
//!
//! impl Task for Calculator {
//!     type Input = String;
//!     type Output = String;
//!     type Error = String;
//!
//!     fn run(&mut self, input: String) -> Result<String, String> {
//!         let num: i32 = input.parse()
//!             .map_err(|e| format!("Parse error: {e}"))?;
//!         Ok(format!("Result: {}", num * 2))
//!     }
//! }
//!
//! fn main() {
//!     tarnish::main::<Calculator>(parent_main);
//! }
//!
//! fn parent_main() {
//!     let mut process = Process::<Calculator>::spawn()
//!         .expect("Failed to spawn process");
//!
//!     match process.call("42".to_string()) {
//!         Ok(result) => println!("Success: {result}"),
//!         Err(e) => eprintln!("Error: {e}"),
//!     }
//! }
//! ```
//!
//! # Clippy Lint Allowances
//!
//! This crate allows certain restrictive lints where they don't add value:
//! - `missing_docs_in_private_items`: Internal implementation details don't need docs
//! - `pattern_type_mismatch`: Pattern matching on internal enums is intentional
//! - `multiple_crate_versions`: Acceptable for development dependencies

// Allow certain restrictive lints that don't add value for this crate
#![allow(
    clippy::missing_docs_in_private_items,
    clippy::pattern_type_mismatch,
    clippy::multiple_crate_versions
)]
#![cfg_attr(doctest, doc = include_str!("../README.md"))]

use std::any;
use std::env;
use std::fmt;
use std::io::{self, Read, Write};
use std::num::NonZeroUsize;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use postcard::accumulator::{CobsAccumulator, FeedResult};

/// Environment variable prefix for child process detection
/// The full variable name is: __`TARNISH_WORKER`_{`TypeName`}__
const WORKER_ENV_PREFIX: &str = "__TARNISH_WORKER_";

/// Check if we're running in any worker context
#[doc(hidden)]
#[must_use]
pub fn is_worker_process() -> bool {
    env::vars().any(|(key, _)| key.starts_with(WORKER_ENV_PREFIX))
}

/// Maximum time to wait for graceful shutdown before sending SIGKILL
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Generate a unique environment variable name for a worker type
fn worker_env_name<T: 'static>() -> String {
    let type_name = any::type_name::<T>()
        .replace("::", "_")
        .replace(['<', '>'], "_");
    format!("{WORKER_ENV_PREFIX}{type_name}__")
}

/// Protocol message types for parent-child communication
#[derive(Debug, serde::Serialize, serde::Deserialize)]
enum Message {
    /// Request from parent to child (contains serialized payload)
    Request(Vec<u8>),
    /// Successful response from child to parent (contains serialized payload)
    Response(Vec<u8>),
    /// Error response from child to parent
    Error(String),
    /// Shutdown signal from parent to child
    Shutdown,
}

impl Message {
    /// Encode message to COBS-encoded bytes with 0x00 terminator
    fn encode(&self) -> io::Result<Vec<u8>> {
        postcard::to_allocvec_cobs(self).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Message encoding failed: {e}"),
            )
        })
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
    /// Task error from child
    TaskError(String),
    /// Protocol error
    ProtocolError(String),
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SpawnError(e) => write!(f, "Failed to spawn process: {e}"),
            Self::ExecutablePathError(e) => {
                write!(f, "Failed to get executable path: {e}")
            }
            Self::CommunicationError(e) => write!(f, "Communication error: {e}"),
            Self::ProcessTerminated => write!(f, "Process terminated unexpectedly"),
            Self::ProcessPanicked(msg) => write!(f, "Process panicked: {msg}"),
            Self::TaskError(msg) => write!(f, "Task error: {msg}"),
            Self::ProtocolError(msg) => write!(f, "Protocol error: {msg}"),
        }
    }
}

impl std::error::Error for ProcessError {}

impl From<io::Error> for ProcessError {
    fn from(err: io::Error) -> Self {
        Self::CommunicationError(err)
    }
}

pub type Result<T> = std::result::Result<T, ProcessError>;

/// Trait for encoding messages to send over process boundaries
///
/// Automatically implemented for all types that implement `serde::Serialize`.
/// Uses postcard with COBS encoding for compact binary serialization.
///
/// **Note**: The serialization format is an implementation detail and may
/// change in future versions for performance or compatibility improvements.
pub trait MessageEncode {
    /// Encode the message to bytes for transmission
    ///
    /// # Errors
    ///
    /// Returns an error if the message cannot be encoded.
    fn encode(&self) -> std::result::Result<Vec<u8>, String>;
}

/// Trait for decoding messages received over process boundaries
///
/// Automatically implemented for all types that implement `serde::Deserialize`.
/// Uses postcard with COBS encoding for compact binary serialization.
pub trait MessageDecode: Sized {
    /// Decode bytes into a message
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes cannot be decoded into the expected type.
    fn decode(bytes: &[u8]) -> std::result::Result<Self, String>;
}

/// Blanket implementation for all `Serialize` types
///
/// Uses postcard with COBS encoding for compact binary serialization
/// with frame delimiters (0x00 bytes).
impl<T: serde::Serialize> MessageEncode for T {
    fn encode(&self) -> std::result::Result<Vec<u8>, String> {
        postcard::to_allocvec_cobs(self).map_err(|e| format!("COBS encoding error: {e}"))
    }
}

/// Blanket implementation for all Deserialize types
impl<T: for<'de> serde::Deserialize<'de>> MessageDecode for T {
    fn decode(bytes: &[u8]) -> std::result::Result<Self, String> {
        // COBS decoding happens in-place, so we need to copy to a mutable buffer
        let mut buf = bytes.to_vec();
        postcard::from_bytes_cobs(&mut buf).map_err(|e| format!("COBS decoding error: {e}"))
    }
}

/// Trait for implementing worker logic that runs in an isolated process
///
/// Implement this trait to define your business logic with your own message types.
///
/// # Example
///
/// ```
/// use tarnish::Task;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Default)]
/// struct MyTask;
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
/// impl Task for MyTask {
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
pub trait Task: Default + 'static {
    /// Input message type (must be encodable for retry and decodable in worker)
    type Input: MessageEncode + MessageDecode;
    /// Output message type (must be encodable in worker and decodable in parent)
    type Output: MessageEncode + MessageDecode;
    /// Error type that can be returned from the worker
    type Error: fmt::Display;

    /// Process a request
    ///
    /// This method is called in the worker process for each request from the parent.
    ///
    /// # Errors
    ///
    /// Returns an error of type `Self::Error` if the task fails.
    fn run(&mut self, input: Self::Input) -> std::result::Result<Self::Output, Self::Error>;
}

/// Handle to a worker process
///
/// Manages the lifecycle of a child process running your [`Task`] implementation.
/// Automatically restarts on panic and performs graceful shutdown when dropped.
///
/// # Example
///
/// ```no_run
/// use tarnish::{Task, Process};
///
/// #[derive(Default)]
/// struct MyTask;
///
/// impl Task for MyTask {
///     type Input = String;
///     type Output = String;
///     type Error = String;
///     fn run(&mut self, input: String) -> Result<String, String> {
///         Ok(format!("Processed: {input}"))
///     }
/// }
///
/// let mut process = Process::<MyTask>::spawn().unwrap();
/// let result = process.call("hello".to_string()).unwrap();
/// ```
pub struct Process<T: Task> {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
    _phantom: std::marker::PhantomData<T>,
}

impl<T: Task> Process<T> {
    /// Spawn a new worker process
    ///
    /// This spawns a new instance of the current binary which should call
    /// `worker_main::<W>()` in its main function.
    ///
    /// # Errors
    ///
    /// Returns an error if the process cannot be spawned or if stdin/stdout cannot be captured.
    pub fn spawn() -> Result<Self> {
        Self::spawn_internal()
    }

    fn spawn_internal() -> Result<Self> {
        let exe_path = env::current_exe().map_err(ProcessError::ExecutablePathError)?;
        let env_name = worker_env_name::<T>();

        let mut child = Command::new(exe_path)
            .env(&env_name, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(ProcessError::SpawnError)?;

        // stdin/stdout are guaranteed to be Some because we piped them
        let stdin = child.stdin.take().ok_or_else(|| {
            ProcessError::SpawnError(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Failed to capture child stdin",
            ))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ProcessError::SpawnError(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Failed to capture child stdout",
            ))
        })?;

        Ok(Self {
            child,
            stdin,
            stdout,
            _phantom: std::marker::PhantomData,
        })
    }

    /// Call the worker with input and wait for response.
    ///
    /// Sends a request to the worker process and waits for a response.
    /// If the worker has crashed, automatically restarts it and returns an error.
    /// The caller can decide whether to retry the operation.
    ///
    /// # Errors
    ///
    /// Returns an error if communication fails, the worker crashes, or the task returns an error.
    #[allow(clippy::needless_pass_by_value)] // We want ownership to prevent reuse of stale data
    pub fn call(&mut self, input: T::Input) -> Result<T::Output> {
        let encoded_input = input
            .encode()
            .map_err(|e| ProcessError::ProtocolError(format!("Failed to encode input: {e}")))?;

        // Send request
        if let Err(e) = self.send_message(&Message::Request(encoded_input)) {
            // Task is likely dead, restart it
            self.restart()?;
            return Err(e);
        }

        // Receive response
        match self.receive_message() {
            Ok(Message::Response(encoded_output)) => {
                // Decode the output
                T::Output::decode(&encoded_output).map_err(|e| {
                    ProcessError::ProtocolError(format!("Failed to decode output: {e}"))
                })
            }
            Ok(Message::Error(err)) => Err(ProcessError::TaskError(err)),
            Ok(msg) => {
                // Unexpected message, restart worker
                self.restart()?;
                Err(ProcessError::ProtocolError(format!(
                    "Unexpected message: {msg:?}"
                )))
            }
            Err(e) => {
                // Communication failed, restart worker
                self.restart()?;
                Err(e)
            }
        }
    }

    fn send_message(&mut self, msg: &Message) -> Result<()> {
        let bytes = msg.encode()?;
        self.stdin.write_all(&bytes)?;
        Ok(())
    }

    fn receive_message(&mut self) -> Result<Message> {
        // Use CobsAccumulator for efficient streaming reads
        let mut raw_buf = [0_u8; 256];
        let mut cobs_buf: CobsAccumulator<1024> = CobsAccumulator::new();

        loop {
            let bytes_read = self.stdout.read(&mut raw_buf)?;

            if bytes_read == 0 {
                return Err(ProcessError::ProcessTerminated);
            }

            let mut window = raw_buf.get(..bytes_read).ok_or_else(|| {
                ProcessError::ProtocolError(format!(
                    "Read returned invalid byte count: {bytes_read} > {}",
                    raw_buf.len()
                ))
            })?;

            while !window.is_empty() {
                window = match cobs_buf.feed::<Message>(window) {
                    FeedResult::Consumed => break,
                    FeedResult::OverFull(remaining) => remaining,
                    FeedResult::DeserError(_remaining) => {
                        return Err(ProcessError::ProtocolError(
                            "COBS deserialization error".to_owned(),
                        ));
                    }
                    FeedResult::Success { data, .. } => {
                        return Ok(data);
                    }
                };
            }
        }
    }

    fn restart(&mut self) -> Result<()> {
        // Kill old child - ignore errors if already dead
        drop(self.child.kill());
        drop(self.child.wait());

        // Spawn new child and replace self with it
        let new_handle = Self::spawn_internal()?;
        *self = new_handle;

        Ok(())
    }

    /// Check if the worker process is still running
    ///
    /// # Errors
    ///
    /// Returns an error if the process status cannot be queried.
    pub fn is_running(&mut self) -> Result<bool> {
        match self.child.try_wait() {
            Ok(Some(_)) => Ok(false),
            Ok(None) => Ok(true),
            Err(e) => Err(ProcessError::CommunicationError(e)),
        }
    }
}

impl<T: Task> Drop for Process<T> {
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
        drop(self.child.kill());
        drop(self.child.wait());
    }
}

/// A pool of worker processes for concurrent task execution.
///
/// `ProcessPool` manages multiple worker processes, distributing tasks across them
/// using round-robin scheduling. Each worker runs in its own isolated process with
/// automatic crash recovery.
///
/// # Example
///
/// ```no_run
/// use std::num::NonZeroUsize;
/// use tarnish::{Task, ProcessPool};
///
/// #[derive(Default)]
/// struct HeavyComputation;
///
/// impl Task for HeavyComputation {
///     type Input = Vec<u8>;
///     type Output = u64;
///     type Error = String;
///
///     fn run(&mut self, input: Vec<u8>) -> Result<u64, String> {
///         // Expensive computation here
///         Ok(input.iter().map(|&x| x as u64).sum())
///     }
/// }
///
/// tarnish::main::<HeavyComputation>(|| {
///     let size = NonZeroUsize::new(4).unwrap();
///     let mut pool = ProcessPool::<HeavyComputation>::new(size)
///         .expect("Failed to create pool");
///
///     // Process 100 items across 4 workers
///     for i in 0..100 {
///         let result = pool.call(vec![i; 1000]);
///         println!("Result {}: {:?}", i, result);
///     }
/// });
/// ```
pub struct ProcessPool<T: Task> {
    workers: Vec<Process<T>>,
    next_worker: std::sync::atomic::AtomicUsize,
}

impl<T: Task> ProcessPool<T> {
    /// Create a new process pool with the specified number of workers.
    ///
    /// Each worker is a separate process that will be spawned immediately.
    /// If any worker fails to spawn, an error is returned and no pool is created.
    ///
    /// # Arguments
    ///
    /// * `size` - The number of worker processes to spawn (must be non-zero).
    ///
    /// # Errors
    ///
    /// Returns an error if any worker process fails to spawn.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::num::NonZeroUsize;
    /// use tarnish::ProcessPool;
    /// # use tarnish::Task;
    /// # #[derive(Default)]
    /// # struct MyTask;
    /// # impl Task for MyTask {
    /// #     type Input = String;
    /// #     type Output = String;
    /// #     type Error = String;
    /// #     fn run(&mut self, input: String) -> Result<String, String> { Ok(input) }
    /// # }
    ///
    /// let size = NonZeroUsize::new(4).unwrap();
    /// let pool = ProcessPool::<MyTask>::new(size)?;
    /// # Ok::<(), tarnish::ProcessError>(())
    /// ```
    pub fn new(size: NonZeroUsize) -> Result<Self> {
        let workers = (0..size.get())
            .map(|_| Process::<T>::spawn())
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            workers,
            next_worker: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Execute a task on the next available worker.
    ///
    /// This method uses round-robin scheduling to distribute work across workers.
    /// The call blocks until the worker returns a result. If the worker crashes,
    /// it will be automatically restarted.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The worker process crashes and cannot be restarted
    /// - Communication with the worker fails
    /// - The worker returns a task error
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use std::num::NonZeroUsize;
    /// # use tarnish::{Task, ProcessPool};
    /// # #[derive(Default)]
    /// # struct MyTask;
    /// # impl Task for MyTask {
    /// #     type Input = String;
    /// #     type Output = String;
    /// #     type Error = String;
    /// #     fn run(&mut self, input: String) -> Result<String, String> { Ok(input) }
    /// # }
    /// let size = NonZeroUsize::new(4).unwrap();
    /// let mut pool = ProcessPool::<MyTask>::new(size)?;
    /// let result = pool.call("hello".to_string())?;
    /// # Ok::<(), tarnish::ProcessError>(())
    /// ```
    #[allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)]
    pub fn call(&mut self, input: T::Input) -> Result<T::Output> {
        let idx = self
            .next_worker
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            % self.workers.len();

        self.workers
            .get_mut(idx)
            .ok_or_else(|| ProcessError::ProtocolError(format!("Invalid worker index: {idx}")))?
            .call(input)
    }

    /// Returns the number of workers in the pool.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use std::num::NonZeroUsize;
    /// # use tarnish::{Task, ProcessPool};
    /// # #[derive(Default)]
    /// # struct MyTask;
    /// # impl Task for MyTask {
    /// #     type Input = String;
    /// #     type Output = String;
    /// #     type Error = String;
    /// #     fn run(&mut self, input: String) -> Result<String, String> { Ok(input) }
    /// # }
    /// let size = NonZeroUsize::new(4).unwrap();
    /// let pool = ProcessPool::<MyTask>::new(size)?;
    /// assert_eq!(pool.size(), 4);
    /// # Ok::<(), tarnish::ProcessError>(())
    /// ```
    #[must_use]
    pub const fn size(&self) -> usize {
        self.workers.len()
    }
}

/// Handle worker process mode in your main function
///
/// Call this at the start of your `main()` function. If it returns `Some(exit_code)`,
/// you're running in worker mode and should exit with that code.
///
/// # Example
///
/// ```no_run
/// use tarnish::{Task, worker_main};
///
/// #[derive(Default)]
/// struct MyTask;
///
/// impl Task for MyTask {
///     type Input = String;
///     type Output = String;
///     type Error = String;
///     fn run(&mut self, input: String) -> Result<String, String> {
///         Ok(format!("Processed: {input}"))
///     }
/// }
///
/// if let Some(exit_code) = worker_main::<MyTask>() {
///     std::process::exit(exit_code);
/// }
/// // Parent process logic here
/// ```
/// Entry point for applications using tarnish.
///
/// This function checks if the current process is running as a worker. If so,
/// it runs the worker loop and exits. If not, it calls the provided function
/// with the parent process logic.
///
/// # Example
///
/// ```no_run
/// use tarnish::{Task, Process};
///
/// #[derive(Default)]
/// struct MyTask;
///
/// impl Task for MyTask {
///     type Input = String;
///     type Output = String;
///     type Error = String;
///
///     fn run(&mut self, input: String) -> Result<String, String> {
///         Ok(input.to_uppercase())
///     }
/// }
///
/// fn parent_main() {
///     let mut process = Process::<MyTask>::spawn()
///         .expect("Failed to spawn worker");
///
///     match process.call("hello".to_string()) {
///         Ok(result) => println!("Result: {result}"),
///         Err(e) => eprintln!("Error: {e}"),
///     }
/// }
///
/// # fn main() {
/// #     tarnish::main::<MyTask>(parent_main);
/// # }
/// ```
pub fn main<T: Task>(parent_main: fn()) {
    let env_name = worker_env_name::<T>();
    if env::var(&env_name).is_ok() {
        // We're in worker mode - run worker loop and exit
        let exit_code = run_worker_loop::<T>();
        std::process::exit(exit_code);
    }

    // We're the parent process - run parent logic
    parent_main();
}

/// Low-level entry point for worker process detection.
///
/// Returns `Some(exit_code)` if running as a worker, `None` if running as parent.
/// Most users should use [`run`] instead, which provides a simpler API.
///
/// # Example
///
/// ```no_run
/// use tarnish::{Task, Process, worker_main};
///
/// # #[derive(Default)]
/// # struct MyTask;
/// # impl Task for MyTask {
/// #     type Input = String;
/// #     type Output = String;
/// #     type Error = String;
/// #     fn run(&mut self, input: String) -> Result<String, String> { Ok(input) }
/// # }
/// if let Some(exit_code) = worker_main::<MyTask>() {
///     std::process::exit(exit_code);
/// }
/// // Parent process logic here
/// ```
#[must_use]
pub fn worker_main<T: Task>() -> Option<i32> {
    let env_name = worker_env_name::<T>();
    if env::var(&env_name).is_err() {
        return None; // Not a worker process
    }

    // We're in worker mode
    let exit_code = run_worker_loop::<T>();
    Some(exit_code)
}

#[allow(clippy::print_stderr)] // Worker process intentionally logs to stderr
fn run_worker_loop<T: Task>() -> i32 {
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    // Create the worker instance
    let mut worker = T::default();

    // CobsAccumulator for receiving messages
    let mut raw_buf = [0_u8; 256];
    let mut cobs_buf: CobsAccumulator<1024> = CobsAccumulator::new();

    loop {
        // Read chunk from parent
        let bytes_read = match stdin.read(&mut raw_buf) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("[CHILD] Failed to read from parent: {e}");
                return 1;
            }
        };

        if bytes_read == 0 {
            // Parent closed stdin, exit gracefully
            return 0;
        }

        let Some(mut window) = raw_buf.get(..bytes_read) else {
            eprintln!(
                "[CHILD] Read returned invalid byte count: {bytes_read} > {}",
                raw_buf.len()
            );
            return 1;
        };

        while !window.is_empty() {
            let message = match cobs_buf.feed::<Message>(window) {
                FeedResult::Consumed => break,
                FeedResult::OverFull(remaining) => {
                    window = remaining;
                    continue;
                }
                FeedResult::DeserError(_remaining) => {
                    eprintln!("[CHILD] COBS deserialization error");
                    return 1;
                }
                FeedResult::Success { data, remaining } => {
                    window = remaining;
                    data
                }
            };

            match message {
                Message::Shutdown => {
                    // Graceful shutdown requested
                    return 0;
                }
                Message::Request(encoded_input) => {
                    // Decode the input
                    let input = match T::Input::decode(&encoded_input) {
                        Ok(inp) => inp,
                        Err(e) => {
                            eprintln!("[WORKER] Failed to decode input: {e}");
                            let err_msg = Message::Error(format!("Decode error: {e}"));
                            if send_message(&mut stdout, &err_msg).is_err() {
                                return 1;
                            }
                            continue;
                        }
                    };

                    // Run business logic - panics will kill this process
                    // and the parent will detect and restart
                    let response = match worker.run(input) {
                        Ok(output) => match output.encode() {
                            Ok(bytes) => Message::Response(bytes),
                            Err(e) => Message::Error(format!("Encoding error: {e}")),
                        },
                        Err(err) => Message::Error(err.to_string()),
                    };

                    if send_message(&mut stdout, &response).is_err() {
                        return 1;
                    }
                }
                Message::Response(_) | Message::Error(_) => {
                    eprintln!("[CHILD] Unexpected message type");
                    return 1;
                }
            }
        }
    }
}

fn send_message(stdout: &mut io::Stdout, msg: &Message) -> io::Result<()> {
    let bytes = msg.encode()?;
    stdout.write_all(&bytes)?;
    stdout.flush()?;
    Ok(())
}

/// Run code in an isolated subprocess with automatic crash recovery
///
/// This macro creates an anonymous task and runs it immediately in a separate process.
/// If the code panics or crashes, the parent process survives.
///
/// # Example
///
/// ```no_run
/// use tarnish::task;
///
/// // Simple task with no input
/// let result = task!(my_task: || -> Result<i32, String> {
///     // This code runs in an isolated subprocess
///     // If it panics, the parent process survives
///     Ok(42)
/// });
///
/// match result {
///     Ok(value) => println!("Success: {}", value),
///     Err(e) => println!("Task failed: {}", e),
/// }
/// ```
///
/// # Usage
///
/// Provide a unique identifier for each task, followed by the closure:
///
/// ```no_run
/// # use tarnish::task;
/// // With explicit return type
/// let result1 = task!(my_task: || -> Result<i32, String> {
///     Ok(42)
/// });
///
/// // Without return type (defaults to tarnish::Result<()>)
/// let result2 = task!(other_task: || {
///     // Do something that might crash
///     Ok(())
/// });
/// ```
#[macro_export]
macro_rules! task {
    // Pattern 1: Without return type - defaults to tarnish::Result<()>
    ($name:ident: || $body:block) => {{
        paste::paste! {
            #[derive(Default)]
            struct [<__TarnishTask $name:camel>];

            impl $crate::Task for [<__TarnishTask $name:camel>] {
                type Input = ();
                type Output = ();
                type Error = $crate::ProcessError;

                fn run(&mut self, _input: ()) -> ::std::result::Result<Self::Output, Self::Error> {
                    (|| $body)()
                }
            }

            // Check if we're the subprocess for this specific task
            if let ::std::option::Option::Some(exit_code) = $crate::worker_main::<[<__TarnishTask $name:camel>]>() {
                ::std::process::exit(exit_code);
            }

            // If we're a worker for a different task, skip this task! call
            // The subprocess will continue until it finds the right task
            if !$crate::is_worker_process() {
                // We're the parent - spawn and run the task
                (|| -> $crate::Result<()> {
                    let mut process = $crate::Process::<[<__TarnishTask $name:camel>]>::spawn()?;
                    process.call(())
                })()
            } else {
                // Worker for different task - this shouldn't be reached as the worker
                // should find its task and exit, but we need to return something
                ::std::result::Result::Err($crate::ProcessError::ProtocolError(
                    "Worker process did not find its task".to_owned()
                ))
            }
        }
    }};

    // Pattern 2: With explicit return type
    ($name:ident: || -> Result<$ok:ty, $err:ty> $body:block) => {{
        paste::paste! {
            #[derive(Default)]
            struct [<__TarnishTask $name:camel>];

            impl $crate::Task for [<__TarnishTask $name:camel>] {
                type Input = ();
                type Output = $ok;
                type Error = $err;

                fn run(&mut self, _input: ()) -> ::std::result::Result<Self::Output, Self::Error> {
                    (|| $body)()
                }
            }

            // Check if we're the subprocess for this specific task
            if let ::std::option::Option::Some(exit_code) = $crate::worker_main::<[<__TarnishTask $name:camel>]>() {
                ::std::process::exit(exit_code);
            }

            // If we're a worker for a different task, skip this task! call
            // The subprocess will continue until it finds the right task
            if !$crate::is_worker_process() {
                // We're the parent - spawn and run the task
                (|| -> $crate::Result<$ok> {
                    let mut process = $crate::Process::<[<__TarnishTask $name:camel>]>::spawn()?;
                    process.call(())
                })()
            } else {
                // Worker for different task - this shouldn't be reached as the worker
                // should find its task and exit, but we need to return something
                ::std::result::Result::Err($crate::ProcessError::ProtocolError(
                    "Worker process did not find its task".to_owned()
                ))
            }
        }
    }};
}
