# tarnish

A library for running code in isolated processes with automatic panic recovery.

## Why?

Sometimes you need to run code that might crash. Maybe you're calling into a C
library through FFI, and somewhere in that library there's a null pointer
dereference in disguise? Or you're loading plugins from users and can't
guarantee they won't panic. Or you're using a third-party sys-crate that wasn't
audited yet.

Rust's type system can't protect you from segfaults in C code. It can't prevent
an `abort()` call in a dependency either. When those things happen, the entire
process terminates.

This library was born out of necessity to handle the specific problem of
wrapping an untrusted sys-crate that made unsafe calls to C. With tarnish, the
dangerous parts run in a separate process. If that crashes, the parent process
detects the failure and spawns a fresh worker.

This pattern turns out to be useful for various kinds of systems programming.
Any code where you can't guarantee stability can be isolated this way, which is
why I generalized the pattern into a reusable library. That said, I haven't used
it specifically outside of my narrow FFI use case yet, so be cautious still.

## Features

- Process-level isolation (survives segfaults/FFI crashes)
- Automatic restart after crashes
- Trait-based API (not macro-based)
- Built-in type-safe message passing
- Production-focused
- General-purpose (any code, not just FFI)

## Under The Hood

The trick is surprisingly simple.

You implement a `Task` trait that encapsulates your risky business logic. When
you spawn a worker, the library creates a fresh copy of your own binary, but
with a special environment variable set. Your `main()` function checks for this
variable at startup. If it's there, you know you're the worker subprocess, and
you should run the worker loop. If it's not there, you're the parent, and you
can spawn workers as needed. 

This is conceptually similar to the classic fork pattern on Unix systems, but it
works on any platform that can spawn processes. The parent and worker
communicate over `stdin` and `stdout`, using messages which get serialized with
[postcard](https://github.com/jamesmunns/postcard), then base64 encoded so they
play nice with text streams. The concrete messaging format is an implementation
detail that you should not rely on, as it can change in future versions.

Serialization and deserialization happens automatically, so contrary to the Unix
fork-exec model, you only need to worry about the business logic.

When a worker panics or crashes, the parent notices immediately. It spawns a
fresh worker and retries the operation. If the crash was transient (cosmic ray,
memory pressure, who knows), the retry succeeds. If it was deterministic (i.e.,
that input will always crash), the retry fails too, and you get an error back.
Either way, your parent process keeps running. 

## Task Trait

The task trait looks like this:

```rust
pub trait Task: Default + 'static {
    type Input: Serialize + Deserialize;
    type Output: Serialize + Deserialize;
    type Error: Display;

    fn run(&mut self, input: Self::Input) -> Result<Self::Output, Self::Error>;
}
```

Your input and output types just need to derive `Serialize` and `Deserialize`.
Everything else happens behind the scenes. You can also use types from the
standard library like `String` for both input and output if that's all you need.
There is a blanket implementation for those.

## Example Use-Case: Wrapping Unsafe FFI

The original use-case is isolating unsafe FFI calls, so let's look at an example
in that context.

```rust
use tarnish::{Task, Process, main};
use serde::{Serialize, Deserialize};

#[derive(Default)]
struct UnsafeFFIWrapper;

// Define your input/output types

#[derive(Serialize, Deserialize)]
struct Input {
    operation: String,
    data: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct Output {
    success: bool,
    data: Vec<u8>,
}

impl Task for UnsafeFFIWrapper {
    type Input = Input;
    type Output = Output;
    type Error = String;

    fn run(&mut self, input: Input) -> Result<Output, String> {
        // This unsafe block might segfault or corrupt memory!
        // But if it does, only this process dies and not the parent.
        unsafe {
            let result = some_unsafe_c_function(
                input.data.as_ptr(),
                input.data.len()
            );

            if result.is_null() {
                return Err("C function returned null".to_string());
            }

            // Process the result...
            Ok(Output {
                success: true,
                data: vec![],
            })
        }
    }
}

fn main() {
    tarnish::main::<UnsafeFFIWrapper>(parent_main);
}

fn parent_main() {
    let mut process = Process::<UnsafeFFIWrapper>::spawn()
        .expect("Failed to spawn worker");

    let input = Input {
        operation: "transform".to_string(),
        data: vec![1, 2, 3, 4],
    };

    match process.call(input) {
        Ok(output) => {
            println!("FFI call succeeded: {:?}", output);
        }
        Err(e) => {
            // If the C code segfaulted, we'll see an error here
            // but our parent process is still running
            eprintln!("Worker crashed or returned error: {}", e);
        }
    }
}

// Your unsafe FFI declaration
extern "C" {
    fn some_unsafe_c_function(data: *const u8, len: usize) -> *mut std::ffi::c_void;
}
```

Note how `main` just calls `tarnish::main()` with the parent logic function.
This handles the check for parent-vs-task automatically.

## Shutdown

When you drop a `Process` handle, it sends a shutdown message to the task and
waits up to 5 seconds. If the task doesn't exit cleanly, it gets a `SIGKILL`.

## When Tasks Crash

When a task crashes mid-operation, `process.call()` automatically restarts the task and returns an error. The fresh task is ready for the next call.

You control the retry logic. Want to retry once?

```rust
// Try once, retry on failure
let result = process.call(input.clone())
    .or_else(|_| process.call(input));
```

Or implement more sophisticated retry logic with backoff, limits, etc. The library handles keeping a fresh task available, you decide when to retry.

## Serialization format

Messages are currently serialized with
[postcard](https://github.com/jamesmunns/postcard), a compact binary format
that's roughly 10-20% the size of JSON. Messages are then base64 encoded before
transmission.

**Why base64?** The parent and task communicate over stdin/stdout using a
line-based protocol (one message per line). Postcard produces binary output
which can contain newline bytes that would break our line delimiter. Base64
encoding ensures messages are text-safe. This adds ~33% overhead but is simple
and correct. The alternative would be length-prefixed framing, which is more
complex.

This is an implementation detail. Future versions might change the format, or
let you pick your own. For now, postcard + base64 does the job well.

If you really don't want the serde dependency, you can disable the default
features and implement `MessageEncode` and `MessageDecode` manually for your
types. But honestly, you probably want serde.

## Notes

Each task type gets its own environment variable based on its type name:
`__TARNISH_WORKER_{TypeName}__`. This means you can have multiple different
task types in the same binary without them stepping on each other's toes.

Tasks must implement `Default` (so we can spawn fresh ones) and be `'static`
(no borrowed data, since they cross process boundaries).

Only tested on macOS and Linux. It probably works on other Unix-like systems.
Windows support would require some work around process spawning and signal
handling.

## Similar Libraries

**[Sandcrust](https://www.researchgate.net/publication/320748351_Sandcrust_Automatic_Sandboxing_of_Unsafe_Components_in_Rust)** - Academic research project for automatic sandboxing of unsafe components
**[rusty-fork](https://crates.io/crates/rusty-fork)** - Process isolation for tests ([GitHub](https://github.com/AltSysrq/rusty-fork))
**[Bastion](https://lib.rs/crates/bastion)** - Fault-tolerant runtime with actor-model supervision
**[rust_supervisor](https://crates.io/crates/rust_supervisor)** - Erlang/OTP-style supervision for Rust threads
**[subprocess](https://crates.io/crates/subprocess)** - Execution and interaction with external processes ([GitHub](https://github.com/hniksic/rust-subprocess))
**[async-process](https://crates.io/crates/async-process)** - Asynchronous process handling

| Feature | tarnish | Sandcrust | rusty-fork | Bastion | rust_supervisor | subprocess/async-process |
|---------|---------|-----------|------------|---------|-----------------|--------------------------|
| Process isolation | ✓ | ✓ | ✓ | ✗ | ✗ | ✓ |
| Automatic restart | ✓ | ✗ | ✗ | ✓ | ✓ | ✗ |
| Survives segfaults | ✓ | ✓ | ✓ | ✗ | ✗ | ✓ |
| Production ready | ✓ | ✗ | ✗ | ✓ | ✓ | ✓ |
| Built-in IPC | ✓ | ✓ | ✗ | ✓ | ✗ | ✗ |
| Trait-based API | ✓ | ✗ | ✗ | ✓ | ✓ | ✗ |
| FFI focus | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ |
| Test isolation | ✗ | ✗ | ✓ | ✗ | ✗ | ✗ |
| Actor model | ✗ | ✗ | ✗ | ✓ | ✗ | ✗ |
| Macro-based | ✗ | ✓ | ✗ | ✗ | ✗ | ✗ |
| External commands | ✗ | ✗ | ✗ | ✗ | ✗ | ✓ |
| Active maintenance | ✓ | ✗ | ✓ | ✓ | ✓ | ✓ |

## About The Name

[Tarnish](https://en.wikipedia.org/wiki/Tarnish) is the protective layer that
forms on metal when it's exposed to air. It looks like damage, but it's actually
protecting the metal underneath from further corrosion. When you scratch it off,
it just grows back.

I think you now understand where I'm going with this. This library is similar.
The worker process is "the tarnish." It takes the hits so your main process
doesn't have to. When it gets damaged, we regenerate it. The protection
continues. This and the Rust pun.
