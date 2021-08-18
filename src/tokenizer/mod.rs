pub mod functions;

use arrayvec::ArrayVec;
use std::fmt;
use std::io;
use std::path::Path;
use std::borrow::Cow;
pub use self::functions::*;

#[derive(Debug)]
pub enum TokenErr {
    File(io::Error),
    OutOfBounds,
}

impl fmt::Display for TokenErr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            TokenErr::File(ref io) => write!(f, "parallel: unable to obtain the Nth input: {}", io),
            TokenErr::OutOfBounds  => write!(f, "parallel: input token out of bounds")
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
/// A token is a placeholder for the operation to be performed on the input value.
pub enum Token {
    /// An argument is simply a collection of characters that are not placeholders.
    Argument(Cow<'static, str>),
    /// Takes the basename (file name) of the input with the extension removed.
    BaseAndExt,
    /// Takes the basename (file name) of the input with a custom suffix removed.
    BaseAndSuffix(&'static str),
    /// Takes the basename (file name) of the input with the directory path removed.
    Basename,
    /// Takes the directory path of the input with the basename removed.
    Dirname,
    /// Returns the job ID of the current input.
    Job,
    /// Takes the input, unmodified.
    Placeholder,
    /// Removes the extension from the input.
    RemoveExtension,
    /// Removes a specified extension pattern
    RemoveSuffix(&'static str),
    /// Returns the thread ID.
    Slot
}

struct Number {
    id: usize,
    token: Token,
}

impl Number {
    fn new(id: usize, token: Token) -> Number {
        Number{ id, token }
    }

    fn into_argument(self, path: &Path) -> Result<String, TokenErr> {
        use std::fs::File;
        use std::io::{BufRead, BufReader};
        let file = File::open(path).map_err(TokenErr::File)?;
        let input = &BufReader::new(file).lines().nth(self.id-1).unwrap().map_err(TokenErr::File)?;
        let argument = match self.token {
            Token::Argument(_)        => unreachable!(),
            Token::Basename           => basename(input),
            Token::BaseAndExt         => basename(remove_extension(input)),
            Token::BaseAndSuffix(pat) => basename(remove_pattern(input, pat)),
            Token::Dirname            => dirname(input),
            Token::Job                => unreachable!(),
            Token::Placeholder        => input,
            Token::RemoveExtension    => remove_extension(input),
            Token::RemoveSuffix(pat)  => remove_pattern(input, pat),
            Token::Slot               => unreachable!()
        };
        Ok(String::from(argument))
    }
}

/// Takes the command arguments as the input and reduces it into tokens,
/// which allows for easier management of string manipulation later on.
pub fn tokenize(tokens: &mut ArrayVec<[Token; 128]>, template: &'static str, path: &Path, nargs: usize)
    -> Result<(), TokenErr>
{
    // When set to true, the characters following will be collected into `pattern`.
    let mut pattern_matching = false;
    // Mark the index where the pattern's first character begins.
    let mut pattern_start = 0;

    // Defines that an argument string is currently being matched
    let mut argument_matching = false;
    // Mark the index where the argument's first character begins.
    let mut argument_start = 0;

    for (id, character) in template.bytes().enumerate() {
        match (character, pattern_matching) {
            // This condition initiates the pattern matching
            (b'{', false) => {
                pattern_matching = true;
                pattern_start    = id;

                // If pattern matching has initialized while argument matching was happening,
                // this will append the argument to the token list and disable argument matching.
                if argument_matching {
                    argument_matching = false;
                    let argument      = Cow::Borrowed(&template[argument_start..id]);
                    tokens.push(Token::Argument(argument));
                }
            },
            // This condition ends the pattern matching process
            (b'}', true)  => {
                pattern_matching = false;
                if id == pattern_start+1 {
                    // This condition will be met when the pattern is "{}".
                    tokens.push(Token::Placeholder);
                } else {
                    // Supply the internal contents of the pattern to the token matcher.
                    match match_token(&template[pattern_start+1..id], path, nargs)? {
                        // If the token is a match, add the matched token.
                        Some(token) => { tokens.push(token); },
                        // If the token is not a match, add it as an argument.
                        None => { tokens.push(Token::Argument(Cow::Borrowed(&template[pattern_start..id+1]))); }
                    }
                }
            },
            // If pattern matching is disabled and argument matching is also disabled,
            // this will begin the argument matching process.
            (_, false) if !argument_matching  => {
                argument_matching = true;
                argument_start    = id;
            },
            (_, _) => ()
        }
    }

    // In the event that there is leftover data that was not matched, this will add the final
    // string to the token list.
    if pattern_matching {
        tokens.push(Token::Argument(Cow::Borrowed(&template[pattern_start..])));
    } else if argument_matching {
        tokens.push(Token::Argument(Cow::Borrowed(&template[argument_start..])));
    }

    Ok(())
}

/// Matches a pattern to it's associated token.
fn match_token(pattern: &'static str, path: &Path, nargs: usize) -> Result<Option<Token>, TokenErr> {
    match pattern {
        "."  => Ok(Some(Token::RemoveExtension)),
        "#"  => Ok(Some(Token::Job)),
        "%"  => Ok(Some(Token::Slot)),
        "/"  => Ok(Some(Token::Basename)),
        "//" => Ok(Some(Token::Dirname)),
        "/." => Ok(Some(Token::BaseAndExt)),
        "##" => Ok(Some(Token::Argument(Cow::Owned(nargs.to_string())))),
        _    => {
            if pattern.starts_with('^') && pattern.len() > 1 {
                Ok(Some(Token::RemoveSuffix(&pattern[1..])))
            } else if pattern.starts_with("/^") && pattern.len() > 2 {
                Ok(Some(Token::BaseAndSuffix(&pattern[2..])))
            } else {
                let ndigits = pattern.bytes().take_while(|&x| (x as char).is_numeric()).count();
                let nchars  = ndigits + pattern.bytes().skip(ndigits).count();
                if ndigits != 0 {
                    let number = pattern[0..ndigits].parse::<usize>().unwrap();
                    if ndigits == nchars {
                        if number == 0 || number > nargs { return Err(TokenErr::OutOfBounds); }
                        let argument = Number::new(number, Token::Placeholder).into_argument(path)?;
                        Ok(Some(Token::Argument(Cow::Owned(argument))))
                    } else {
                        match match_token(&pattern[ndigits..], path, nargs)? {
                            None | Some(Token::Job) |  Some(Token::Slot) => Ok(None),
                            Some(token) => {
                                let argument = Number::new(number, token).into_argument(path)?;
                                Ok(Some(Token::Argument(Cow::Owned(argument))))
                            },
                        }
                    }
                } else {
                    Ok(None)
                }
            }
        }
    }
}

// TODO: Fix Tests
// #[test]
// fn tokenizer_argument() {
//     let mut tokens = Vec::new();
//     let _ = tokenize(&mut tokens, "foo", &Path::new("."), 1);
//     assert_eq!(tokens, vec![Token::Argument("foo".to_owned())]);
// }

// #[test]
// fn tokenizer_placeholder() {
//     let mut tokens = Vec::new();
//     let _ = tokenize(&mut tokens, "{}", &Path::new("."), 1);
//     assert_eq!(tokens, vec![Token::Placeholder]);
// }

// #[test]
// fn tokenizer_remove_extension() {
//     let mut tokens = Vec::new();
//     let _ = tokenize(&mut tokens, "{.}", &Path::new("."), 1);
//     assert_eq!(tokens, vec![Token::RemoveExtension]);
// }

// #[test]
// fn tokenizer_basename() {
//     let mut tokens = Vec::new();
//     let _ = tokenize(&mut tokens, "{/}", &Path::new("."), 1);
//     assert_eq!(tokens, vec![Token::Basename]);
// }

// #[test]
// fn tokenizer_dirname() {
//     let mut tokens = Vec::new();
//     let _ = tokenize(&mut tokens, "{//}", &Path::new("."), 1);
//     assert_eq!(tokens, vec![Token::Dirname]);
// }

// #[test]
// fn tokenizer_base_and_ext() {
//     let mut tokens = Vec::new();
//     let _ = tokenize(&mut tokens, "{/.}", &Path::new("."), 1);
//     assert_eq!(tokens, vec![Token::BaseAndExt]);
// }

// #[test]
// fn tokenizer_slot() {
//     let mut tokens = Vec::new();
//     let _ = tokenize(&mut tokens, "{%}", &Path::new("."), 1);
//     assert_eq!(tokens, vec![Token::Slot]);
// }

// #[test]
// fn tokenizer_job() {
//     let mut tokens = Vec::new();
//     let _ = tokenize(&mut tokens, "{#}", &Path::new("."), 1);
//     assert_eq!(tokens, vec![Token::Job]);
// }

// #[test]
// fn tokenizer_multiple() {
//     let mut tokens = Vec::new();
//     let _ = tokenize(&mut tokens, "foo {} bar", &Path::new("."), 1);
//     assert_eq!(tokens, vec![Token::Argument("foo ".to_owned()), Token::Placeholder,
//         Token::Argument(" bar".to_owned())]);
// }

// #[test]
// fn tokenizer_no_space() {
//     let mut tokens = Vec::new();
//     let _ = tokenize(&mut tokens, "foo{}bar", &Path::new("."), 1);
//     assert_eq!(tokens, vec![Token::Argument("foo".to_owned()), Token::Placeholder,
//         Token::Argument("bar".to_owned())]);
// }
