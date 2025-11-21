use tarnish::{worker_main, Worker, Process};

// Worker that panics on specific input
#[derive(Default)]
struct PanickingWorker {
    call_count: usize,
}

impl Worker for PanickingWorker {
    type Input = String;  // Use String (has blanket impl)
    type Output = String;
    type Error = String;

    fn run(&mut self, input: String) -> Result<String, String> {
        self.call_count += 1;
        eprintln!(
            "[WORKER] Call #{}: Processing '{}'",
            self.call_count, input
        );

        match input.as_str() {
            "panic" => {
                eprintln!("[WORKER] About to panic!");
                panic!("Intentional panic for testing!");
            }
            "error" => {
                eprintln!("[WORKER] Returning error");
                Err("Business logic error".to_string())
            }
            "divide_by_zero" => {
                eprintln!("[WORKER] About to divide by zero!");
                #[allow(unconditional_panic)]
                let _ = 1 / 0; // This will panic
                Ok("Never reached".to_string())
            }
            _ => {
                let result = format!("Processed: {}", input);
                eprintln!("[WORKER] Returning: {}", result);
                Ok(result)
            }
        }
    }
}

fn main() {
    if let Some(exit_code) = worker_main::<PanickingWorker>() {
        std::process::exit(exit_code);
    }

    println!("=== Panic Recovery Example ===\n");
    println!("This demonstrates automatic process restart on panic\n");

    let mut process = Process::<PanickingWorker>::spawn()
        .expect("Failed to spawn process");

    let test_cases = vec![
        ("hello", "Should succeed"),
        ("panic", "Will panic and auto-restart"),
        ("world", "Should succeed after restart"),
        ("divide_by_zero", "Another panic"),
        ("error", "Business logic error"),
        ("final", "Final successful call"),
    ];

    for (input, description) in test_cases {
        println!("Test: {} - {}", input, description);

        match process.call(input.to_string()) {
            Ok(result) => println!("  ✓ Success: {}\n", result),
            Err(e) => println!("  ✗ Error: {}\n", e),
        }

        // Small delay to make output more readable
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    println!("=== All tests completed ===");
    println!("Note: Worker process was restarted automatically after panics");
    println!("Parent process remained stable throughout");
}
