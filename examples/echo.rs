use serde::{Deserialize, Serialize};
use tarnish::{Process, Task};

// Define custom input/output types with automatic serialization
#[derive(Debug, Clone, Serialize, Deserialize)]
enum Input {
    Echo(String),
    Repeat(String, usize),
}

#[derive(Debug, Serialize, Deserialize)]
enum Output {
    Message(String),
}

// Define the task
#[derive(Default)]
struct EchoTask;

impl Task for EchoTask {
    type Input = Input;
    type Output = Output;
    type Error = String;

    fn run(&mut self, input: Input) -> Result<Output, String> {
        eprintln!("[WORKER] Processing: {:?}", input);

        let result = match input {
            Input::Echo(msg) => format!("Echo: {}", msg),
            Input::Repeat(msg, count) => std::iter::repeat(&msg)
                .take(count)
                .cloned()
                .collect::<Vec<_>>()
                .join(" "),
        };

        Ok(Output::Message(result))
    }
}

fn main() {
    tarnish::main::<EchoTask>(parent_main);
}

fn parent_main() {
    println!("[PARENT] Starting echo example with automatic serde serialization\n");

    let mut process = Process::<EchoTask>::spawn().expect("Failed to spawn process");

    // Test different input types - serialization is automatic!
    let inputs = vec![
        Input::Echo("Hello".to_string()),
        Input::Echo("World".to_string()),
        Input::Repeat("Hi".to_string(), 3),
        Input::Repeat("Rust".to_string(), 5),
    ];

    for input in inputs {
        println!("[PARENT] Sending: {:?}", input);

        match process.call(input) {
            Ok(Output::Message(msg)) => println!("[PARENT] Received: {}\n", msg),
            Err(e) => eprintln!("[PARENT] Error: {}\n", e),
        }
    }

    println!("[PARENT] Done - process will gracefully shutdown on drop");
}
