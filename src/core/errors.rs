use std::fmt;

pub const EXIT_OK: i32 = 0;
pub const EXIT_DASHBOARD_ERROR: i32 = 1;
pub const EXIT_VALIDATION_ERROR: i32 = 2;
pub const EXIT_NOT_FOUND: i32 = 3;
pub const EXIT_CONFLICT: i32 = 4;
pub const EXIT_IO_ERROR: i32 = 5;

#[derive(Debug, Clone)]
pub struct WbError {
    pub exit_code: i32,
    pub message: String,
}

impl WbError {
    pub fn new(exit_code: i32, message: impl Into<String>) -> Self {
        Self {
            exit_code,
            message: message.into(),
        }
    }

    pub fn dashboard(message: impl Into<String>) -> Self {
        Self::new(EXIT_DASHBOARD_ERROR, message)
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(EXIT_VALIDATION_ERROR, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(EXIT_NOT_FOUND, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(EXIT_CONFLICT, message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(EXIT_IO_ERROR, message)
    }
}

impl fmt::Display for WbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for WbError {}
