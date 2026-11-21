use std::{fmt, io};

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum Code {
    Ok = 0,
    Runtime = 1,
    Usage = 2,
    Diff = 3,
    Data = 65,
    Missing = 69,
    Create = 73,
    Timeout = 75,
}

#[derive(Debug)]
pub struct Error(pub Code, pub String);
pub type Result<T> = std::result::Result<T, Error>;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.1)
    }
}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self(Code::Runtime, e.to_string())
    }
}
pub fn data(s: impl Into<String>) -> Error {
    Error(Code::Data, s.into())
}
pub fn usage(s: impl Into<String>) -> Error {
    Error(Code::Usage, s.into())
}
