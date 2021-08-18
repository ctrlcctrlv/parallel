use arguments;
use execute::command;
use input_iterator::{InputIterator, InputIteratorErr};
use numtoa::NumToA;
use tokenizer::Token;

use std::io::{self, Read, StdoutLock, Write};

/// Instead of executing commands in parallel, the commands that would be
/// executed will be printed directly to the standard output of this
/// application. This also applies to shell quoted arguments.
pub fn dry_run<IO: Read>(flags: u16, inputs: InputIterator<IO>, arguments: &[Token]) {
    let stdout = io::stdout();
    let stdout = &mut stdout.lock();
    let stderr = io::stderr();
    let stderr = &mut stderr.lock();
    let mut command_buffer = String::new();
    let slot = "{SLOT_ID}";
    let pipe = flags & arguments::PIPE_IS_ENABLED != 0;
    let mut id_buffer = [0u8; 20];
    let mut total_buffer = [0u8; 20];
    let start_indice = inputs.total_arguments.numtoa(10, &mut total_buffer);
    let job_total = &total_buffer[start_indice..];

    // If `SHELL_QUOTE` is enabled then the quoted command will be printed,
    // otherwise the command will be printed unmodified. The correct function to
    // execute will be assigned here in advance.
    let pipe_action: Box<dyn Fn(&mut StdoutLock, &str)> = if flags & arguments::SHELL_QUOTE != 0 {
        Box::new(|stdout: &mut StdoutLock, input: &str| {
            if let Some(new_arg) = shell_quote(input) {
                let _ = stdout.write(new_arg.as_bytes());
            } else {
                let _ = stdout.write(input.as_bytes());
            }
        })
    } else {
        Box::new(|stdout: &mut StdoutLock, input: &str| {
            let _ = stdout.write(input.as_bytes());
        })
    };

    for (job_id, input) in inputs.enumerate() {
        match input {
            Ok(input) => {
                let start_indice = job_id.numtoa(10, &mut id_buffer);
                let command = command::ParallelCommand {
                    slot_no: slot,
                    job_no: &id_buffer[start_indice..],
                    job_total,
                    input: &input,
                    command_template: arguments,
                    flags,
                };

                command.build_arguments(&mut command_buffer);
                if !pipe {
                    command::append_argument(
                        &mut command_buffer,
                        command.command_template,
                        command.input,
                    );
                }
                pipe_action(stdout, &command_buffer);
                let _ = stdout.write(b"\n");
                command_buffer.clear();
            },
            Err(why) => match why {
                InputIteratorErr::FileRead(path, why) => {
                    let _ = writeln!(
                        stderr,
                        "parallel: input file read error: {:?}: {}",
                        path, why
                    );
                },
            },
        }
    }
}

/// Simply escapes special characters, optionally returning a new `String` if
/// changes occurred
fn shell_quote(command: &str) -> Option<String> {
    // Determines if allocations will be necessary or not.
    let mut needs_escaping = false;
    for character in command.chars() {
        match character {
            '$' | ' ' | '\\' | '>' | '<' | '^' | '&' | '#' | '!' | '*' | '\'' | '\"' | '`'
            | '~' | '{' | '}' | '[' | ']' | '(' | ')' | ';' | '|' | '?' => needs_escaping = true,
            _ => (),
        }
    }

    if needs_escaping {
        let mut output = String::with_capacity(command.len() * 2);
        for character in command.chars() {
            match character {
                '$' | ' ' | '\\' | '>' | '<' | '^' | '&' | '#' | '!' | '*' | '\'' | '\"' | '`'
                | '~' | '{' | '}' | '[' | ']' | '(' | ')' | ';' | '|' | '?' => output.push('\\'),
                _ => (),
            }
            output.push(character);
        }
        Some(output)
    } else {
        None
    }
}
