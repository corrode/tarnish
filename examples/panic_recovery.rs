use tarnish::{Process, Task};

// Task that panics on specific input
#[derive(Default)]
struct PanickingTask {
    call_count: usize,
}

impl Task for PanickingTask {
    type Input = String; // Use String (has blanket impl)
    type Output = String;
    type Error = String;

    fn run(&mut self, input: String) -> Result<String, String> {
        self.call_count += 1;
        eprintln!("[WORKER] Call #{}: Processing '{}'", self.call_count, input);

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
    tarnish::main::<PanickingTask>(parent_main);
}

fn parent_main() {
    println!("Panic Recovery Example\n");
    println!("This demonstrates automatic restart with manual retry\n");

    let mut process = Process::<PanickingTask>::spawn().expect("Failed to spawn process");

    let test_cases = vec![
        ("hello", "Should succeed"),
        ("panic", "Will panic and auto-restart"),
        ("world", "Should succeed after auto-restart"),
        ("divide_by_zero", "Another panic"),
        ("error", "Business logic error"),
        ("final", "Final successful call"),
    ];

    for (input, description) in test_cases {
        println!("Test: {} - {}", input, description);

        // Task restarts automatically on crash, we control retry logic
        let result = process.call(input.to_string()).or_else(|e| {
            println!("  First attempt failed: {}", e);
            println!("  Retrying...");
            // Task was auto-restarted, just retry
            process.call(input.to_string())
        });

        match result {
            Ok(result) => println!("  ✓ Success: {}\n", result),
            Err(e) => println!("  ✗ Error: {}\n", e),
        }

        // Small delay to make output more readable
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    println!("All tests completed");
    println!("Note: Task was automatically restarted after each crash");
    println!("Parent process remained stable throughout");
}
