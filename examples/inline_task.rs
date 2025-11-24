// Examples are allowed to use print statements and debug formatting
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::use_debug,
    clippy::expect_used,
    clippy::missing_docs_in_private_items,
    clippy::panic,
    clippy::shadow_unrelated,
    clippy::exit
)]

use tarnish::task;

fn main() {
    eprintln!("Inline Task Macro Example\n");

    // Task with explicit return type
    eprintln!("Test 1: Task with explicit return type");
    let result = task!(calculation: || -> Result<i32, String> {
        let x = 10;
        let y = 32;
        Ok(x + y)
    });

    match result {
        Ok(value) => eprintln!("  ✓ Success: {value}\n"),
        Err(e) => eprintln!("  ✗ Error: {e}\n"),
    }

    // Task with default return type (Result<(), ProcessError>)
    eprintln!("Test 2: Task with default return type");
    let result = task!(simple: || {
        eprintln!("  Running simple task...");
        Ok(())
    });

    match result {
        Ok(()) => eprintln!("  ✓ Success\n"),
        Err(e) => eprintln!("  ✗ Error: {e}\n"),
    }

    // Task that panics - parent survives
    eprintln!("Test 3: Task that panics");
    let result = task!(panic_test: || -> Result<i32, String> {
        panic!("This panic is caught by process isolation!");
    });

    match result {
        Ok(value) => eprintln!("  ✓ Success: {value}\n"),
        Err(e) => eprintln!("  ✗ Error: {e}\n"),
    }

    eprintln!("All tests completed!");
    eprintln!("Parent process remained stable throughout");
}
