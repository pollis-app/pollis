//! Pollis stand-in for crates.io `device-info` 0.1.1 (#1320).
//!
//! LiveKit's Rust SDK 0.9 (`livekit-signaling` 0.1.6, `get_livekit_url`) puts
//! `device_info::device_info().map(|i| i.model).unwrap_or_default()` into every
//! signal join: as `ClientInfo.device_model` inside the `/rtc/v1` protobuf join
//! request, and as the `device_model` query parameter on the `/rtc` (v0)
//! fallback. On desktop that is the SMBIOS product name (a motherboard or laptop
//! model such as "MS-7E59"), the Windows registry equivalent, or the Mac model
//! identifier. The SDK offers no option to leave it out.
//!
//! The SFU operator is untrusted for metadata in our model, and a hardware model
//! is a fingerprinting signal with no product use, so this crate answers every
//! query with `Unsupported`. The SDK's `unwrap_or_default()` then sends an empty
//! string, which protobuf drops from the wire entirely. It also never reads the
//! hostname (`DeviceInfo::name`), which the real crate collects alongside.
//!
//! The public API mirrors upstream 0.1.1 (Apache-2.0, LiveKit, Inc.) for the
//! items the dependency graph uses. Upstream's Android-only `android::init` is
//! omitted: nothing here calls it (mobile builds pollis-core without the `media`
//! feature, so the Rust LiveKit client is never linked there).
//!
//! When moving livekit to a release that bumps `device-info`, bump this crate's
//! version with it, or the `[patch.crates-io]` entry stops applying.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DeviceType {
    Desktop,
    Laptop,
    Phone,
    Tablet,
    Headset,
    Television,
    Watch,
    Unknown,
}

impl fmt::Display for DeviceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            DeviceType::Desktop => "Desktop",
            DeviceType::Laptop => "Laptop",
            DeviceType::Phone => "Phone",
            DeviceType::Tablet => "Tablet",
            DeviceType::Headset => "Headset",
            DeviceType::Television => "Television",
            DeviceType::Watch => "Watch",
            DeviceType::Unknown => "Unknown",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub model: String,
    pub name: String,
    pub device_type: DeviceType,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DeviceInfoError {
    #[error("platform not supported")]
    Unsupported,
    #[error("failed to query device info: {0}")]
    Query(String),
}

/// Always `Err(Unsupported)`: Pollis does not disclose the device model, name or
/// type to anything that asks.
pub fn device_info() -> Result<DeviceInfo, DeviceInfoError> {
    Err(DeviceInfoError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact expression `livekit-signaling` 0.1.6 sends as the device model.
    #[test]
    fn the_sdk_expression_yields_an_empty_model() {
        let device_model = device_info().map(|info| info.model).unwrap_or_default();
        assert_eq!(device_model, "");
    }
}
