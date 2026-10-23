use std::fmt;

/// What can go wrong talking to a hardware wallet. Every variant carries words a person can act on,
/// because the UI shows them as they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The device is not there any more, or was never found.
    NotFound,
    /// Another program (or another window of this one) holds the device.
    Busy(String),
    /// The device wants a PIN or a passphrase that has not been given.
    Locked(String),
    /// The device has no seed on it.
    Uninitialised,
    /// The person said no on the device, or cancelled here.
    Declined(String),
    /// The device is set to another network than the wallet.
    Network(String),
    /// This device cannot do what was asked.
    Unsupported(String),
    /// The device said something this code does not understand.
    Protocol(String),
    /// The cable, the port or the driver failed.
    Io(String),
    /// The PSBT cannot be signed as given.
    Psbt(String),
    /// A device answered, but it is not the one this wallet belongs to.
    Mismatch(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound => f.write_str("no device found, check the cable and unlock it"),
            Error::Busy(s) => write!(f, "device is in use by another program: {s}"),
            Error::Locked(s) => write!(f, "device is locked: {s}"),
            Error::Uninitialised => f.write_str("the device has no wallet on it, set it up first"),
            Error::Declined(s) => write!(f, "declined: {s}"),
            Error::Network(s) => write!(f, "wrong network: {s}"),
            Error::Unsupported(s) => write!(f, "not supported: {s}"),
            Error::Protocol(s) => write!(f, "unexpected reply: {s}"),
            Error::Io(s) => write!(f, "device error: {s}"),
            Error::Psbt(s) => write!(f, "cannot sign this transaction: {s}"),
            Error::Mismatch(s) => write!(f, "wrong device: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    pub fn cancelled() -> Error {
        Error::Declined("cancelled".into())
    }

    pub fn is_cancel(&self) -> bool {
        matches!(self, Error::Declined(_))
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::PermissionDenied => Error::Busy(format!(
                "no permission to open it ({e}), on Linux install the udev rules and join the plugdev or dialout group"
            )),
            std::io::ErrorKind::NotFound => Error::NotFound,
            _ => Error::Io(e.to_string()),
        }
    }
}

impl From<bitcoin::psbt::Error> for Error {
    fn from(e: bitcoin::psbt::Error) -> Self {
        Error::Psbt(e.to_string())
    }
}

impl From<bitcoin::bip32::Error> for Error {
    fn from(e: bitcoin::bip32::Error) -> Self {
        Error::Protocol(format!("bad key: {e}"))
    }
}
