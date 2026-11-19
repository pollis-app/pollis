//! What a handler and a host call can fail with.

use std::fmt;

/// `Result<T, pollis_app::Error>`. A handler returns `pollis_app::Result`.
pub type Result<T = (), E = Error> = std::result::Result<T, E>;

/// Why a call or a handler failed.
///
/// Returning one from a handler fails the job: nothing it queued is sent and
/// no state is written. `?` converts any standard error into this type.
pub struct Error {
    message: String,
    refused: bool,
}

impl Error {
    /// An error with this message. [`bail!`](crate::bail) is the short form.
    pub fn msg(message: impl fmt::Display) -> Error {
        Error {
            message: message.to_string(),
            refused: false,
        }
    }

    pub(crate) fn refused(message: String) -> Error {
        Error {
            message,
            refused: true,
        }
    }

    /// True when the runtime refused a call. The message names the manifest
    /// field that refused it, e.g. "`http-fetch` to `api.x.com` refused: not
    /// in `egress`".
    pub fn is_refused(&self) -> bool {
        self.refused
    }

    /// The error's text.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.message)
    }
}

// `Error` deliberately does not implement `std::error::Error`: that is what
// lets this blanket conversion exist beside the standard `From<T> for T`.
impl<E: std::error::Error> From<E> for Error {
    fn from(error: E) -> Error {
        Error::msg(error)
    }
}

/// Return early from a handler with an error: `bail!("no ticket {id}")`.
#[macro_export]
macro_rules! bail {
    ($($message:tt)*) => {
        return ::core::result::Result::Err($crate::Error::msg(::std::format!($($message)*)))
    };
}
