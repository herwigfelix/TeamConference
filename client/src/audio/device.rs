use cpal::traits::{DeviceTrait, HostTrait};
use serde::Serialize;

// Für eine künftige Geräteauswahl im UI vorgehalten.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub name: String,
    pub is_input: bool,
    pub is_output: bool,
    pub is_default: bool,
}

/// List all available audio input and output devices.
#[allow(dead_code)]
pub fn list_devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let mut devices = Vec::new();

    let default_input_name = host
        .default_input_device()
        .and_then(|d| d.name().ok());
    let default_output_name = host
        .default_output_device()
        .and_then(|d| d.name().ok());

    if let Ok(input_devices) = host.input_devices() {
        for device in input_devices {
            if let Ok(name) = device.name() {
                let is_default = default_input_name.as_deref() == Some(&name);
                devices.push(DeviceInfo {
                    name: name.clone(),
                    is_input: true,
                    is_output: false,
                    is_default,
                });
            }
        }
    }

    if let Ok(output_devices) = host.output_devices() {
        for device in output_devices {
            if let Ok(name) = device.name() {
                let is_default = default_output_name.as_deref() == Some(&name);
                // Check if this device is already in the list as input
                if let Some(existing) = devices.iter_mut().find(|d| d.name == name) {
                    existing.is_output = true;
                    existing.is_default = existing.is_default || is_default;
                } else {
                    devices.push(DeviceInfo {
                        name,
                        is_input: false,
                        is_output: true,
                        is_default,
                    });
                }
            }
        }
    }

    devices
}

/// Find an input device by name, or return the default.
pub fn get_input_device(name: Option<&str>) -> Result<cpal::Device, String> {
    let host = cpal::default_host();

    if let Some(name) = name {
        if let Ok(devices) = host.input_devices() {
            for device in devices {
                if device.name().ok().as_deref() == Some(name) {
                    return Ok(device);
                }
            }
        }
    }

    host.default_input_device()
        .ok_or_else(|| "No input device available".to_string())
}

/// Find an output device by name, or return the default.
pub fn get_output_device(name: Option<&str>) -> Result<cpal::Device, String> {
    let host = cpal::default_host();

    if let Some(name) = name {
        if let Ok(devices) = host.output_devices() {
            for device in devices {
                if device.name().ok().as_deref() == Some(name) {
                    return Ok(device);
                }
            }
        }
    }

    host.default_output_device()
        .ok_or_else(|| "No output device available".to_string())
}
