#![deny(dead_code)]
#![allow(unknown_lints)]
extern crate arrayvec;
extern crate itoa;
extern crate numtoa;
extern crate num_cpus;
extern crate permutate;
extern crate smallvec;
extern crate sys_info;
extern crate time;
extern crate wait_timeout;

mod arguments;
mod disk_buffer;
mod execute;
mod filepaths;
mod input_iterator;
mod misc;
mod tokenizer;
mod shell;
mod verbose;

use std::env;
use std::fs::{self, create_dir_all, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::mem;
use std::process::exit;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::sync::{Arc, Mutex};
use std::sync::mpsc::channel;

use arguments::Args;
use execute::pipe::disk::State;
use input_iterator::{InputIterator, InputsLock};
use tokenizer::{Token, tokenize};

/// The command string needs to be available in memory for the entirety of the application, so this
/// is achievable by transmuting the lifetime of the reference into a static lifetime. To guarantee
/// that this is perfectly safe, and that the reference will live outside the scope, the value will
/// also be leaked so that it is forced to remain in memory for the remainder of the application.
unsafe fn leak_string(comm: String) -> &'static str {
    let new_comm = mem::transmute(&comm as &str);
    mem::forget(comm);
    new_comm
}

/// The tokens will live throughout the entirety of the application, so it's OK to mark it with
/// a static lifetime. Prevents needing to copy the token vector to each thread.
unsafe fn static_arg(args: &[Token]) -> &'static [Token] { mem::transmute(args) }

fn main() {
    // Obtain a handle to standard output/error's buffers so we can write directly to them.
    let stdout = io::stdout();
    let stderr = io::stderr();

    // On Linux systems, check if transparent_hugepages is set to always and issue a warning if true.
    if cfg!(target_os = "linux") {
        if let Ok(mut file) = File::open("/sys/kernel/mm/transparent_hugepage/enabled") {
            let mut buffer: [u8; 2] = [0, 0];
            if let Ok(_) = file.read_exact(&mut buffer) {
                if &buffer == b"[a" {
                    let _ = writeln!(stderr.lock(), "ion: /sys/kernel/mm/transparent_hugepage/enabled is set to always instead of madvise. This will gravely effect the performance of Parallel.");
                }
            }
        }
    }

    // Parse arguments and collect flags and statistics.
    let mut args      = Args::new();
    let mut comm      = String::with_capacity(128);
    let raw_arguments = env::args().collect::<Vec<String>>();

    // Attempt to obtain the default tempdir base path.
    let mut base  = match filepaths::base() {
        Some(base) => base,
        None => {
            let mut stderr = stderr.lock();
            let _ = stderr.write(b"parallel: unable to open home directory");
            exit(1);
        }
    };

    // Create the base directory if it does not exist
    if let Err(why) = create_dir_all(&base) {
        let stderr = &mut stderr.lock();
        let _ = writeln!(stderr, "parallel: unable to create tempdir {:?}: {}", base, why);
        exit(1);
    }

    // Collect the command, arguments, and tempdir base path.
    args.ninputs = match args.parse(&mut comm, &raw_arguments, &mut base) {
        Ok(inputs) => inputs,
        Err(why) => why.handle(&raw_arguments)
    };

    // Attempt to convert the base path into a string slice.
    let base_path = match base.to_str() {
        Some(base) => String::from(base),
        None => {
            let stderr = &mut stderr.lock();
            let _ = writeln!(stderr, "parallel: tempdir path, {:?}, is invalid", base);
            exit(1);
        }
    };

    // Construct the paths of each of the required files using the base tempdir path.
    // These paths will be shared all through the application to avoid needing to copy.
    let mut unprocessed_path = base.clone();
    let mut processed_path   = base.clone();
    let mut errors_path      = base;
    unprocessed_path.push("unprocessed");
    processed_path.push("processed");
    errors_path.push("errors");

    // This file is required by the upcoming `InputIterator`. It will remain open for the
    // remainder of the application.
    let file = match fs::OpenOptions::new().read(true).open(&unprocessed_path) {
        Ok(file) => file,
        Err(why) => {
            let stderr = &mut stderr.lock();
            let _ = writeln!(stderr, "parallel: unable to open unprocessed file: {:?}: {}", &unprocessed_path, why);
            exit(1);
        }
    };

    // Initialize the `InputIterator` structure, which efficiently generates inputs from the
    // above `unprocessed` file until all arguments have been processed, denoted by `args.ninputs`.
    let inputs = InputIterator::new(&unprocessed_path, file, args.ninputs)
        .expect("unable to initialize the InputIterator structure");

    // Coerce the `comm` `String` into a `&'static str` so that it may be shared by all threads.
    // This is safe because the original `comm` may no longer be modified due to shadowing rules.
    // It is also safe because `comm` lives to the end of the program.
    let static_comm = unsafe { leak_string(comm) };

    // Attempt to tokenize the command argument into simple primitive placeholders.
    if let Err(error) = tokenize(&mut args.arguments, static_comm, &unprocessed_path, args.ninputs) {
        let stderr = &mut stderr.lock();
        let _ = writeln!(stderr, "{}", error);
        exit(1)
    }

    // Coerce the arguments into a `&'static [Token]` to eliminate the need to make a copy in
    // each thread.
    let arguments = unsafe { static_arg(&args.arguments) };

    // If the `--dry-run` parameter was passsed, the program will simply print all commands to
    // execute and will subsequently quit. Otherwise, real work will be performed.
    if args.flags & arguments::DRY_RUN != 0 {
        execute::dry_run(args.flags, inputs, arguments);
    } else {


        // The `InputIterator` will be wrapped within a `Mutex` so that it can safely be shared
        // across all of the upcoming threads. A `Mutex` is required because each time a thread
        // pulls the next input from the queue, it needs to update various data fields which
        // rely on threads waiting for their turn to report back.
        let shared_input = Arc::new(Mutex::new(inputs));

        // This channel is used exclusively for signaling back to the main thread when a task
        // has completed or has errored.
        let (output_tx, input_rx) = channel::<State>();

        // Will contain handles to the upcoming threads to know when the threads are finished.
        let mut threads = Vec::with_capacity(args.ncores);

        if args.flags & arguments::VERBOSE_MODE != 0 {
            verbose::total_inputs(&stdout, args.ncores, args.ninputs);
        }

        // The `slot` variable is required by the {%} token.
        if args.flags & arguments::INPUTS_ARE_COMMANDS != 0 {
            if shell::ion_exists() {
                args.flags |= arguments::ION_EXISTS;
            } else if shell::dash_exists() {
                args.flags |= arguments::DASH_EXISTS;
            }

            for _ in 0..args.ncores {
                let flags = args.flags;

                let mut exec = execute::ExecInputs {
                    num_inputs: args.ninputs,
                    timeout:    args.timeout,
                    output_tx:  output_tx.clone(),
                    tempdir:    base_path.clone(),
                    inputs:     InputsLock {
                        inputs:    shared_input.clone(),
                        memory:    args.memory,
                        delay:     args.delay,
                        has_delay: args.delay != Duration::from_millis(0),
                        completed: false,
                        flags,
                    }
                };

                let handle: JoinHandle<()> = thread::spawn(move || exec.run(flags));

                // Add the thread handle to the `threads` vector to know when to quit the program.
                threads.push(handle);
            }
        } else {
            shell::set_flags(&mut args.flags, arguments);

            for slot in 1..args.ncores+1 {
                let timeout    = args.timeout;
                let num_inputs = args.ninputs;
                let output_tx  = output_tx.clone();
                let flags      = args.flags;
                let base_path  = base_path.clone();

                let inputs = InputsLock {
                    inputs:    shared_input.clone(),
                    memory:    args.memory,
                    delay:     args.delay,
                    has_delay: args.delay != Duration::from_millis(0),
                    completed: false,
                    flags,
                };

                // The command will be built from the arguments, and inputs will be transferred to the command.
                let handle: JoinHandle<()> = thread::spawn(move || {
                    let mut exec = execute::ExecCommands {
                        slot,
                        num_inputs,
                        flags,
                        timeout,
                        inputs,
                        output_tx,
                        arguments,
                        tempdir:    base_path,
                    };
                    exec.run();
                });

                // Add the thread handle to the `threads` vector to know when to quit the program.
                threads.push(handle);
            }
        }

        /// Prints messages from executed commands in the correct order.
        let errors = execute::receive_messages(input_rx, args, &base_path, &processed_path, &errors_path);

        /// Wait for all threads to exit before proceeding.
        for thread in threads { thread.join().unwrap(); }

        // If errors have occurred, re-print these errors at the end.
        if let Ok(file) = File::open(errors_path) {
            if file.metadata().ok().map_or(0, |metadata| metadata.len()) > 0 {
                let stderr = &mut stderr.lock();
                let _ = stderr.write(b"parallel: encountered errors during processing:\n");
                for line in BufReader::new(file).lines() {
                    if let Ok(line) = line {
                        let _ = stderr.write(line.as_bytes());
                        let _ = stderr.write(b"\n");
                    }
                }
                exit(errors);
            }
        }
    }
}
