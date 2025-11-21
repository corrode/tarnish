use tarnish::{worker_main, Worker, Process};

// Worker for potentially dangerous computations
#[derive(Default)]
struct Calculator;

impl Worker for Calculator {
    type Input = String;   // Use String (has blanket impl)
    type Output = String;
    type Error = String;

    fn run(&mut self, input: String) -> Result<String, String> {
        // Try to parse as number
        let num: i32 = input
            .parse()
            .map_err(|_| format!("Cannot parse '{}' as number", input))?;

        // Division by zero would panic without proper handling
        if num == 0 {
            return Err("Division by zero".to_string());
        }

        let result = 100 / num;
        Ok(format!("{}", result))
    }
}

fn main() {
    if let Some(exit_code) = worker_main::<Calculator>() {
        std::process::exit(exit_code);
    }

    println!("=== Safe Computation Example ===");
    println!("Running potentially dangerous computations in isolation\n");

    let mut process = Process::<Calculator>::spawn()
        .expect("Failed to spawn process");

    let test_cases = vec![
        ("10", "Valid: 100 / 10"),
        ("0", "Error: Division by zero"),
        ("abc", "Error: Invalid input"),
        ("42", "Valid: 100 / 42"),
        ("-5", "Valid: 100 / -5"),
    ];

    for (input, description) in test_cases {
        println!("Testing: {} ({})", input, description);

        match process.call(input.to_string()) {
            Ok(result) => println!("  ✓ Result: 100 / {} = {}\n", input, result),
            Err(e) => println!("  ✗ Error: {}\n", e),
        }
    }

    println!("=== All computations completed safely ===");
}
