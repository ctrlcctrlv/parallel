use super::InputIteratorErr;
use arguments::errors::FileErr;
use disk_buffer::*;
use itoa;
use std::{
    io::{self, Read, Write},
    path::Path,
    str,
};
use time;

#[allow(clippy::upper_case_acronyms)]
pub struct ETA {
    pub left:    u64,
    pub time:    u64,
    pub average: u64,
}

impl ETA {
    pub fn write_to_stderr(&self, completed: usize) {
        let stderr = io::stderr();
        let mut stderr = stderr.lock();

        // Write the ETA to the standard error
        let _ = stderr.write(b"ETA: ");
        let _ = itoa::write(&mut stderr, self.time / 1_000_000_000);

        // Write the number of inputs left to process to standard error
        let _ = stderr.write(b"s Left: ");
        let _ = itoa::write(&mut stderr, self.left);

        // Write the average runtime of processes (with two decimal places) to standard
        // error
        let _ = stderr.write(b" AVG: ");
        let _ = itoa::write(&mut stderr, self.average / 1_000_000_000);
        let _ = stderr.write(b".");
        let remainder = (self.average % 1_000_000_000) / 10_000_000;
        let _ = itoa::write(&mut stderr, remainder);
        if remainder < 10 {
            let _ = stderr.write(b"0");
        }

        // Write the number of completed units so far to standard error
        let _ = stderr.write(b"s Completed: ");
        let _ = itoa::write(&mut stderr, completed);
        let _ = stderr.write(b"\n");
    }
}

/// The `InputIterator` tracks the total number of arguments, the current
/// argument counter, and takes ownership of an `InputBuffer` which buffers
/// input arguments from the disk when arguments stored in memory are depleted.
pub struct InputIterator<IO: Read> {
    pub total_arguments: usize,
    pub curr_argument:   usize,
    pub completed:       usize,
    start_time:          u64,
    average_time:        u64,
    input_buffer:        InputBuffer<IO>,
}

impl<IO: Read> InputIterator<IO> {
    pub fn new(path: &Path, file: IO, args: usize) -> Result<InputIterator<IO>, FileErr> {
        // Create an `InputBuffer` from the unprocessed file.
        let disk_buffer = DiskBufferReader::new(path, file);

        let input_buffer = InputBuffer::new(disk_buffer)?;

        Ok(InputIterator {
            total_arguments: args,
            curr_argument: 0,
            completed: 0,
            input_buffer,
            start_time: time::precise_time_ns(),
            average_time: 0,
        })
    }

    fn buffer(&mut self) -> Result<(), InputIteratorErr> {
        // Read the next set of arguments from the unprocessed file, but only read as
        // many bytes as the buffer can hold without overwriting the unused
        // bytes that was shifted to the left.
        self.input_buffer
            .disk_buffer
            .buffer(self.input_buffer.capacity)
            .map_err(|why| {
                InputIteratorErr::FileRead(self.input_buffer.disk_buffer.path.clone(), why)
            })?;
        let bytes_read = self.input_buffer.disk_buffer.capacity;

        // Update the recorded number of arguments and indices.
        self.input_buffer.start = self.input_buffer.end + 1;
        count_arguments(&mut self.input_buffer, bytes_read);
        self.input_buffer.index = 0;
        Ok(())
    }

    pub fn eta(&self) -> ETA {
        let left = self.total_arguments as u64 - self.completed as u64;
        ETA {
            left,
            time: left * self.average_time,
            average: self.average_time,
        }
    }

    pub fn next_value(&mut self, buffer: &mut String) -> Option<Result<(), InputIteratorErr>> {
        if self.curr_argument == self.total_arguments {
            // If all arguments have been depleted, return `None`.
            return None;
        } else if self.curr_argument == self.input_buffer.end {
            // If the next argument is not stored in the internal buffer, update the buffer.
            if let Err(err) = self.buffer() {
                return Some(Err(err));
            }
        }

        // Obtain the start and end indices to know where to find the input in the
        // array.
        let end = self.input_buffer.indices[self.input_buffer.index + 1];
        let start = if self.input_buffer.index == 0 {
            self.input_buffer.indices[self.input_buffer.index]
        } else {
            self.input_buffer.indices[self.input_buffer.index] + 1
        };

        // Update times
        match self.completed {
            0 => (),
            1 => self.average_time = time::precise_time_ns() - self.start_time,
            _ =>
                self.average_time =
                    (time::precise_time_ns() - self.start_time) / self.completed as u64,
        }

        // Increment the iterator's state.
        self.curr_argument += 1;
        self.input_buffer.index += 1;

        // Copy the input from the buffer into a `String` and return it
        buffer.truncate(0);
        unsafe {
            buffer.push_str(str::from_utf8_unchecked(
                &self.input_buffer.disk_buffer.data[start..end],
            ));
        }
        Some(Ok(()))
    }
}

// Implement the `Iterator` trait for `InputIterator` to gain access to all the
// `Iterator` methods for free.
impl<IO: Read> Iterator for InputIterator<IO> {
    type Item = Result<String, InputIteratorErr>;

    fn next(&mut self) -> Option<Result<String, InputIteratorErr>> {
        if self.curr_argument == self.total_arguments {
            // If all arguments have been depleted, return `None`.
            return None;
        } else if self.curr_argument == self.input_buffer.end {
            // If the next argument is not stored in the internal buffer, update the buffer.
            if let Err(err) = self.buffer() {
                return Some(Err(err));
            }
        }

        // Obtain the start and end indices to know where to find the input in the
        // array.
        let end = self.input_buffer.indices[self.input_buffer.index + 1];
        let start = if self.input_buffer.index == 0 {
            self.input_buffer.indices[self.input_buffer.index]
        } else {
            self.input_buffer.indices[self.input_buffer.index] + 1
        };

        // Update times
        match self.completed {
            0 => (),
            1 => self.average_time = time::precise_time_ns() - self.start_time,
            _ =>
                self.average_time =
                    (time::precise_time_ns() - self.start_time) / self.completed as u64,
        }

        // Increment the iterator's state.
        self.curr_argument += 1;
        self.input_buffer.index += 1;

        // Copy the input from the buffer into a `String` and return it
        Some(Ok(String::from_utf8_lossy(
            &self.input_buffer.disk_buffer.data[start..end],
        )
        .into_owned()))
    }
}

/// Higher level buffer implementation which keeps track of how many inputs are
/// currently stored in the buffer, where all of the indices of the input
/// delimiters are, and which segment of the complete set of arguments are
/// currently buffered.
struct InputBuffer<IO: Read> {
    index:       usize,
    start:       usize,
    end:         usize,
    capacity:    usize,
    disk_buffer: DiskBufferReader<IO>,
    indices:     [usize; BUFFER_SIZE / 2],
}

impl<IO: Read> InputBuffer<IO> {
    /// Takes ownership of a `DiskBufferReader` and transforms it into a higher
    /// level `InputBuffer` which will track additional information about
    /// the disk buffer.
    fn new(mut unprocessed: DiskBufferReader<IO>) -> Result<InputBuffer<IO>, FileErr> {
        unprocessed
            .buffer(0)
            .map_err(|why| FileErr::Read(unprocessed.path.clone(), why))?;
        let bytes_read = unprocessed.capacity;

        let mut temp = InputBuffer {
            index:       0,
            start:       0,
            end:         0,
            capacity:    0,
            disk_buffer: unprocessed,
            indices:     [0usize; BUFFER_SIZE / 2],
        };

        count_arguments(&mut temp, bytes_read);
        Ok(temp)
    }
}

/// Counts the number of arguments that are stored in the buffer, marking the
/// location of the indices and the actual capacity of the buffer's useful
/// information.
fn count_arguments<IO: Read>(buffer: &mut InputBuffer<IO>, bytes_read: usize) {
    let mut newlines = 1;
    buffer.capacity = 0;

    for (indice, _) in buffer
        .disk_buffer
        .data
        .iter()
        .take(bytes_read)
        .enumerate()
        .filter(|&(_, byte)| *byte == b'\n')
    {
        buffer.indices[newlines] = indice;
        newlines += 1;
    }

    newlines -= 1;
    buffer.capacity = buffer.indices[newlines];
    buffer.end += newlines;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn test_input_iterator() {
        let file = File::open("tests/buffer.dat").unwrap();
        let iterator = InputIterator::new(Path::new("tests/buffer.dat"), file, 4096).unwrap();
        assert_eq!(0, iterator.input_buffer.start);
        assert_eq!(1859, iterator.input_buffer.end);
        for (actual, expected) in iterator.zip(1..4096) {
            assert_eq!(actual.unwrap(), expected.to_string());
        }
    }
}
