use std::{
    any::Any,
    ffi::NulError,
    fmt::{self, Display},
    io,
    net::{Ipv4Addr, Ipv6Addr},
    str::FromStr,
};

use pem::Pem;
use thiserror::Error;
use uuid::Uuid;

use crate::{ServerState, ServerVersion, mac::MacAddress, stream::control::PacketSendError};

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    // --- Moonlight ---
    // -- Session Configuration
    #[error("hdr not supported")]
    SettingHdrNotSupported,
    #[error("4k not supported")]
    Setting4kNotSupported,
    #[error("4k not supported: Your device must support HEVC or AV1 to stream at 4k")]
    Setting4kNotSupportedCodecMissing,
    #[error("4k not supported: Update GeForce Experience")]
    Setting4kNotSupportedUpdateGfe,
    // -- HTTP / HTTPS request and responses
    #[error(
        "the https client doesn't have the required credentials, set them before making a request that requires authentication"
    )]
    Unauthenticated,
    #[error("couldn't establish a connection")]
    ConnectionFailed,
    #[error("couldn't establish a connection because of a timeout")]
    ConnectionTimeout,
    #[error("the host returned an unsuccessful status code({code}): {reason}")]
    StatusCode { code: i32, reason: String },
    // -- Pairing
    #[error("another device is currently pairing with the server")]
    PairingFailedAlreadyInProgress,
    #[error("failed to pair because the pin was incorrect")]
    PairingFailedWrongPin,
    #[error("failed")]
    PairingFailed,
    #[error("this action requires pairing")]
    NotPaired,
    // -- Moonlight Shared
    #[error("the server version is not supported: {0}")]
    ServerVersionNotSupported(ServerVersion),
    #[error("failed to send packet: {0}")]
    PacketSend(#[from] PacketSendError),
    // -- Moonlight Common C
    #[error("couldn't aquire an instance")]
    InstanceAquire,
    #[error("a connection is already active")]
    ConnectionAlreadyExists,
    #[error("an error happened whilst sending an event")]
    EventSendError(i32),
    #[error("this call requires a GFE version which uses ENet")]
    ENetRequired,
    #[error("a string contained a nul byte which is not allowed in c strings")]
    StringNulError(#[from] NulError),
    // -- Crypto
    #[error("failed to decrypt data: {0}")]
    DecryptFailed(&'static str),
    // --- Parsing ---
    #[error("session parse")]
    Session(#[from] sdp_types::ParserError),
    #[error("{context}: missing required attribute {attribute:?}")]
    MissingAttribute {
        context: &'static str,
        attribute: &'static str,
    },
    #[error("{context}: invalid attribute for {attribute:?}: got {got:?}, expected {expected}")]
    InvalidAttribute {
        context: &'static str,
        attribute: &'static str,
        expected: &'static str,
        got: String,
    },
    #[error("{context}: failed to parse value: got {got:?}, expected {expected}")]
    InvalidValue {
        context: &'static str,
        expected: &'static str,
        got: String,
    },
    #[error("invalid mode: {0}")]
    InvalidVideoMode(String),
    #[error("{context}: missing payload")]
    MissingPayload { context: &'static str },
    #[error("link header is missing the uri")]
    MissingUri,
    #[error("invalid link header")]
    InvalidLinkHeader,
    // --- Other ---
    #[error("a lock has been poisoned")]
    LockPoisoned,
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("failed to join a thread: {0}")]
    ThreadPanic(String),
    #[error("other: {0}")]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
    #[error("multiple errors occured: {0}")]
    List(ErrorList),
}

impl Error {
    #[allow(unused)]
    pub(crate) fn from_thread_panic(error: Box<dyn Any + Send>) -> Self {
        let message = if let Some(message) = error.downcast_ref::<&str>() {
            (*message).to_owned()
        } else if let Some(message) = error.downcast_ref::<String>() {
            message.clone()
        } else {
            "unknown panic payload".to_owned()
        };

        Error::ThreadPanic(message)
    }

    #[allow(unused)]
    pub(crate) fn other(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Other(error.into())
    }
}

impl<T> From<std::sync::PoisonError<T>> for Error {
    fn from(_value: std::sync::PoisonError<T>) -> Self {
        Self::LockPoisoned
    }
}

impl From<ErrorList> for Error {
    fn from(value: ErrorList) -> Self {
        // try to flatten the list if possible
        if value.0.len() == 1 {
            value
                .0
                .into_iter()
                .next()
                .expect("cannot extract first element of error list with length 1")
        } else {
            Self::List(value)
        }
    }
}

#[derive(Debug)]
pub struct ErrorList(Vec<Error>);

impl From<ErrorList> for Vec<Error> {
    fn from(value: ErrorList) -> Self {
        value.0
    }
}

#[derive(Debug, Error)]
#[error("the error list must be non empty")]
pub struct ErrorListEmpty;

impl TryFrom<Vec<Error>> for ErrorList {
    type Error = ErrorListEmpty;

    fn try_from(value: Vec<Error>) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(ErrorListEmpty);
        }

        Ok(Self(value))
    }
}

impl Display for ErrorList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "multiple errors occurred:")?;

        for (i, error) in self.0.iter().enumerate() {
            writeln!(f, "  {}. {}", i + 1, error)?;
        }

        Ok(())
    }
}

impl std::error::Error for ErrorList {}

#[allow(unused)]
pub(crate) fn parse_error(
    context: &'static str,
    attribute: impl Into<Option<&'static str>>,
    expected: &'static str,
    value: String,
) -> Error {
    if let Some(attribute) = attribute.into() {
        Error::InvalidAttribute {
            context,
            attribute,
            expected,
            got: value,
        }
    } else {
        Error::InvalidValue {
            context,
            expected,
            got: value,
        }
    }
}

#[allow(unused)]
pub(crate) fn parse_u32(
    context: &'static str,
    attribute: impl Into<Option<&'static str>>,
    value: &str,
) -> Result<u32, Error> {
    value.parse().map_err(|_| {
        parse_error(
            context,
            attribute,
            "a valid positive integer or 0 (u32)",
            value.to_string(),
        )
    })
}
#[allow(unused)]
pub(crate) fn parse_u16(
    context: &'static str,
    attribute: impl Into<Option<&'static str>>,
    value: &str,
) -> Result<u16, Error> {
    value.parse().map_err(|_| {
        parse_error(
            context,
            attribute,
            "a valid positive integer or 0 (u16)",
            value.to_string(),
        )
    })
}

#[allow(unused)]
pub(crate) fn parse_i32(
    context: &'static str,
    attribute: impl Into<Option<&'static str>>,
    value: &str,
) -> Result<i32, Error> {
    value.parse().map_err(|_| {
        parse_error(
            context,
            attribute,
            "a valid integer (i32)",
            value.to_string(),
        )
    })
}

#[allow(unused)]
pub(crate) fn parse_number_as_bool(
    context: &'static str,
    attribute: &'static str,
    value: &str,
) -> Result<bool, Error> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(Error::InvalidAttribute {
            context,
            attribute,
            expected: "0 or 1",
            got: value.to_string(),
        }),
    }
}

#[allow(unused)]
pub(crate) fn parse_ipv4(
    context: &'static str,
    attribute: impl Into<Option<&'static str>>,
    value: &str,
) -> Result<Ipv4Addr, Error> {
    value.parse().map_err(|_| {
        parse_error(
            context,
            attribute,
            "a valid ipv4 address",
            value.to_string(),
        )
    })
}

#[allow(unused)]
pub(crate) fn parse_ipv6(
    context: &'static str,
    attribute: impl Into<Option<&'static str>>,
    value: &str,
) -> Result<Ipv6Addr, Error> {
    value.parse().map_err(|_| {
        parse_error(
            context,
            attribute,
            "a valid ipv6 address",
            value.to_string(),
        )
    })
}

#[allow(unused)]
pub(crate) fn parse_mac(
    context: &'static str,
    attribute: impl Into<Option<&'static str>>,
    value: &str,
) -> Result<MacAddress, Error> {
    value
        .parse()
        .map_err(|_| parse_error(context, attribute, "a valid mac address", value.to_string()))
}

#[allow(unused)]
pub(crate) fn parse_uuid(
    context: &'static str,
    attribute: impl Into<Option<&'static str>>,
    value: &str,
) -> Result<Uuid, Error> {
    value
        .parse()
        .map_err(|_| parse_error(context, attribute, "a valid uuid", value.to_string()))
}

#[allow(unused)]
pub(crate) fn parse_server_version(
    context: &'static str,
    attribute: impl Into<Option<&'static str>>,
    value: &str,
) -> Result<ServerVersion, Error> {
    value.parse().map_err(|_| {
        parse_error(
            context,
            attribute,
            "a valid sunshine server version",
            value.to_string(),
        )
    })
}

#[allow(unused)]
pub(crate) fn parse_hex(
    context: &'static str,
    attribute: impl Into<Option<&'static str>>,
    value: &str,
) -> Result<Vec<u8>, Error> {
    hex::decode(value)
        .map_err(|_| parse_error(context, attribute, "valid hex bytes", value.to_string()))
}

#[allow(unused)]
pub(crate) fn parse_pem(
    context: &'static str,
    attribute: impl Into<Option<&'static str>>,
    value: &str,
) -> Result<Pem, Error> {
    Pem::from_str(value)
        .map_err(|_| parse_error(context, attribute, "a valid pem string", value.to_string()))
}

#[allow(unused)]
pub(crate) fn parse_server_state(
    context: &'static str,
    attribute: impl Into<Option<&'static str>>,
    value: &str,
) -> Result<ServerState, Error> {
    ServerState::from_str(value).map_err(|_| {
        parse_error(
            context,
            attribute,
            "a valid server state",
            value.to_string(),
        )
    })
}
