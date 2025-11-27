#![allow(
    clippy::print_stderr,
    clippy::use_debug,
    clippy::missing_docs_in_private_items,
    clippy::unwrap_used
)]

use std::num::NonZeroUsize;
use std::thread;
use std::time::Duration;
use tarnish::{ProcessPool, Task};

#[derive(Default)]
struct HeavyComputation;

impl Task for HeavyComputation {
    type Input = usize;
    type Output = u64;
    type Error = String;

    fn run(&mut self, input: usize) -> Result<u64, String> {
        let pid = std::process::id();
        eprintln!("[Worker PID {pid}] Starting computation for input {input}");

        // Simulate heavy computation with sleep
        thread::sleep(Duration::from_millis(500));

        let result: u64 = (0..input).map(|x| x as u64).sum();
        eprintln!("[Worker PID {pid}] Completed: {result}");
        Ok(result)
    }
}

fn main() {
    tarnish::main::<HeavyComputation>(|| {
        eprintln!("Creating pool with 4 workers...");

        let size = NonZeroUsize::new(4).unwrap();
        let mut pool = match ProcessPool::<HeavyComputation>::new(size) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Failed to create pool: {e}");
                return;
            }
        };

        let pool_size = pool.size();
        eprintln!("Pool size: {pool_size}\n");

        // Process 12 tasks across 4 workers
        // Each worker should get ~3 tasks due to round-robin
        eprintln!("Processing 12 tasks across {pool_size} workers:\n");

        for i in 1_usize..=12 {
            eprintln!("[Parent] Submitting task {i}");

            // Use saturating_mul to avoid arithmetic overflow
            let input = i.saturating_mul(100);

            match pool.call(input) {
                Ok(result) => eprintln!("[Parent] Task {i} result: {result}\n"),
                Err(e) => eprintln!("[Parent] Task {i} failed: {e}\n"),
            }
        }

        eprintln!("All tasks completed!");
    });
}
