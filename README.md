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
detects the failure and spawns a new worker.

This pattern turns out to be useful for various kinds of systems programming.
Any code where you can't guarantee stability can be isolated this way, which is
why I generalized the pattern into a reusable library. That said, I haven't used
it specifically outside of my narrow FFI use case yet, so be cautious still.

## Under The Hood

The trick is surprisingly simple.

You implement a `Worker` trait that encapsulates your risky business logic. When
you spawn a worker, the library creates a fresh copy of your own binary, but
with a special environment variable set. Your `main()` function checks for this
variable at startup. If it's there, you know you're the worker subprocess, and
you should run the worker logic. If it's not there, you're the parent, and you
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

## Worker Trait

The worker trait looks like this:

```rust
pub trait Worker: Default + 'static {
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
use tarnish::{Worker, Process, run};
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

impl Worker for UnsafeFFIWrapper {
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
    run::<UnsafeFFIWrapper, _>(parent_main);
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

Note how `main` just calls `tarnish::run()` with the actual parent logic as a
closure. This handles the check for parent-vs-worker automatically.

## Shutdown

When you drop a `Process` handle, it sends a shutdown message to the worker and
waits up to 5 seconds. If the worker doesn't exit cleanly, it gets a `SIGKILL`.

## When Workers Die

TODO!: This section is outdated if we don't handle retries anymore. Fix that.

Here's what happens when a worker crashes mid-operation:

1. The parent tries to read the output and gets nothing, but the pipe is broken and the worker is gone.
2. The parent spawns a fresh worker process and retries the operation. If it works this time, great! Your code gets the result like nothing happened. If it crashes again, the parent gives up and returns an
error to your code.

This retry-once strategy handles transient failures gracefully (maybe the worker
was unlucky with memory allocation, maybe the C library had a bad day), while
still surfacing persistent problems (your input deterministically triggers a
segfault). You get the best of both worlds: resilience against flukes, but no
infinite retry loops masking real bugs.

## Serialization format

Messages are currently serialized with
[postcard](https://github.com/jamesmunns/postcard), a compact binary format
that's roughly 10-20% the size of JSON. It's fast, simple, and works well for
our purposes. Messages get base64 encoded before being sent over stdin/stdout,
because binary data and text streams don't always play nicely together.

This is an implementation detail. Future versions might change the format, or
let you pick your own. For now, postcard does the job well.

If you really don't want the serde dependency, you can disable the default
features and implement `MessageEncode` and `MessageDecode` manually for your
types. But honestly, you probably want serde.

## Notes

Each worker type gets its own environment variable based on its type name:
`__TARNISH_WORKER_{TypeName}__`. This means you can have multiple different
worker types in the same binary without them stepping on each other's toes.

Workers must implement `Default` (so we can spawn fresh ones) and be `'static`
(no borrowed data, since they cross process boundaries). Auto-restart currently
retries exactly once. Maybe that should be configurable. It isn't yet.

Only tested on macOS and Linux. It probably works on other Unix-like systems.
Windows support would require some work around process spawning and signal
handling.

## About the name

[Tarnish](https://en.wikipedia.org/wiki/Tarnish) is the protective layer that
forms on metal when it's exposed to air. It looks like damage, but it's actually
protecting the metal underneath from further corrosion. When you scratch it off,
it just grows back.

I think you now understand where I'm going with this. This library is similar.
The worker process is "the tarnish." It takes the hits so your main process
doesn't have to. When it gets damaged, we regenerate it. The protection
continues. This and the Rust pun.
