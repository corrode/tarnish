// Examples are allowed to use print statements and debug formatting
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::use_debug,
    clippy::expect_used,
    clippy::missing_docs_in_private_items,
    clippy::arithmetic_side_effects,
    clippy::map_err_ignore
)]

use tarnish::{Process, Task};

// Task for potentially dangerous computations
#[derive(Default)]
struct Calculator;

impl Task for Calculator {
    type Input = String; // Use String (has blanket impl)
    type Output = String;
    type Error = String;

    fn run(&mut self, input: String) -> Result<String, String> {
        // Try to parse as number
        let num: i32 = input
            .parse()
            .map_err(|_| format!("Cannot parse '{input}' as number"))?;

        // Division by zero would panic without proper handling
        if num == 0 {
            return Err("Division by zero".to_owned());
        }

        let result = 100 / num;
        Ok(format!("{result}"))
    }
}

fn main() {
    tarnish::main::<Calculator>(parent_main);
}

fn parent_main() {
    println!("Safe Computation Example");
    println!("Running potentially dangerous computations in isolation\n");

    let mut process = Process::<Calculator>::spawn().expect("Failed to spawn process");

    let test_cases = vec![
        ("10", "Valid: 100 / 10"),
        ("0", "Error: Division by zero"),
        ("abc", "Error: Invalid input"),
        ("42", "Valid: 100 / 42"),
        ("-5", "Valid: 100 / -5"),
    ];

    for (input, description) in test_cases {
        println!("Testing: {input} ({description})");

        match process.call(input.to_owned()) {
            Ok(result) => println!("  ✓ Result: 100 / {input} = {result}\n"),
            Err(e) => println!("  ✗ Error: {e}\n"),
        }
    }

    println!("All computations completed safely");
}
