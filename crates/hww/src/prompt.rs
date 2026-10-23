/// What the person has to do on the device right now.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Notice {
    /// Talking to the device, nothing for the person to do.
    Working,
    /// Waiting for the PIN to be typed on the device.
    PinOnDevice,
    /// Waiting for the passphrase to be typed on the device.
    PassphraseOnDevice,
    /// Waiting for the person to approve something on the device screen. The text says what.
    Confirm(String),
}

/// A passphrase answer.
#[derive(Clone, PartialEq, Eq)]
pub enum Passphrase {
    /// Typed on the computer.
    Host(String),
    /// Typed on the device.
    Device,
}

impl std::fmt::Debug for Passphrase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Passphrase::Host(_) => f.write_str("Passphrase::Host(..)"),
            Passphrase::Device => f.write_str("Passphrase::Device"),
        }
    }
}

/// How a blocking call reaches the person.
///
/// It is called from the thread that is talking to the device, and `pin_matrix` and `passphrase` may
/// block that thread until the person answers. `cancelled` is polled while the device is the
/// one being waited for, which is what makes a PIN prompt something that can be walked away from.
pub trait Prompt {
    fn notify(&self, notice: Notice);

    /// The Trezor One style PIN: the digits of the positions pressed on the keypad grid.
    /// `None` cancels.
    fn pin_matrix(&self) -> Option<String>;

    /// `None` cancels.
    fn passphrase(&self) -> Option<Passphrase>;

    fn cancelled(&self) -> bool;
}

/// A prompt for tests and scripts: says nothing, answers nothing, never cancels.
pub struct Silent;

impl Prompt for Silent {
    fn notify(&self, _: Notice) {}

    fn pin_matrix(&self) -> Option<String> {
        None
    }

    fn passphrase(&self) -> Option<Passphrase> {
        Some(Passphrase::Host(String::new()))
    }

    fn cancelled(&self) -> bool {
        false
    }
}
