use tarnish::{worker_main, Process, Worker};
use serde::{Deserialize, Serialize};

// Define custom message types with automatic serialization
#[derive(Debug, Clone, Serialize, Deserialize)]
enum Request {
    Echo(String),
    Repeat(String, usize),
}

#[derive(Debug, Serialize, Deserialize)]
enum Response {
    Message(String),
}

// Define the worker
#[derive(Default)]
struct EchoWorker;

impl Worker for EchoWorker {
    type Input = Request;
    type Output = Response;
    type Error = String;

    fn run(&mut self, input: Request) -> Result<Response, String> {
        eprintln!("[WORKER] Processing: {:?}", input);

        let result = match input {
            Request::Echo(msg) => format!("Echo: {}", msg),
            Request::Repeat(msg, count) => {
                std::iter::repeat(&msg)
                    .take(count)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ")
            }
        };

        Ok(Response::Message(result))
    }
}

fn main() {
    // Handle worker process mode
    if let Some(exit_code) = worker_main::<EchoWorker>() {
        std::process::exit(exit_code);
    }

    // Parent process logic
    println!("[PARENT] Starting echo example with automatic serde serialization\n");

    let mut process = Process::<EchoWorker>::spawn()
        .expect("Failed to spawn process");

    // Test different message types - serialization is automatic!
    let requests = vec![
        Request::Echo("Hello".to_string()),
        Request::Echo("World".to_string()),
        Request::Repeat("Hi".to_string(), 3),
        Request::Repeat("Rust".to_string(), 5),
    ];

    for req in requests {
        println!("[PARENT] Sending: {:?}", req);

        match process.call(req) {
            Ok(Response::Message(msg)) => println!("[PARENT] Received: {}\n", msg),
            Err(e) => eprintln!("[PARENT] Error: {}\n", e),
        }
    }

    println!("[PARENT] Done - process will gracefully shutdown on drop");
}
