use crate::settings::{AppSettings, AppSettingsPatch, AudioDeviceCatalog, AudioDeviceOption};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextMenuModel {
    pub sections: Vec<ContextMenuSection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextMenuSection {
    pub id: String,
    pub label: String,
    pub items: Vec<ContextMenuItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextMenuItem {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub checked: bool,
}

impl AppSettings {
    /// Projects only the audio quick-control submenus into the tray menu; the
    /// tray builder adds window entries such as Open Settings itself.
    pub fn menu_model(&self, audio_devices: &AudioDeviceCatalog) -> ContextMenuModel {
        let sections = vec![
            audio_device_section(
                "microphone-input",
                "Microphone Input",
                "input",
                &audio_devices.inputs,
                self.input_device_id.as_deref(),
                "Loading microphones...",
            ),
            audio_device_section(
                "speaker-output",
                "Speaker Output",
                "output",
                &audio_devices.outputs,
                self.output_device_id.as_deref(),
                "Loading speakers...",
            ),
        ];
        ContextMenuModel { sections }
    }

    /// Resolves one quick-control menu item into an application patch.
    pub fn patch_for_menu_item(
        &self,
        audio_devices: &AudioDeviceCatalog,
        item_id: &str,
    ) -> Option<AppSettingsPatch> {
        if let Some(device_id) = selected_audio_device_patch(audio_devices, item_id, "input") {
            return Some(AppSettingsPatch::InputDevice(device_id));
        }
        if let Some(device_id) = selected_audio_device_patch(audio_devices, item_id, "output") {
            return Some(AppSettingsPatch::OutputDevice(device_id));
        }
        None
    }
}

fn audio_device_section(
    id: &str,
    label: &str,
    prefix: &str,
    devices: &[AudioDeviceOption],
    selected_id: Option<&str>,
    loading_label: &str,
) -> ContextMenuSection {
    let mut items = vec![ContextMenuItem {
        id: format!("{prefix}:default"),
        label: "System default".to_string(),
        enabled: true,
        checked: selected_id.is_none(),
    }];
    if devices.is_empty() {
        items.push(ContextMenuItem {
            id: format!("{prefix}:loading"),
            label: loading_label.to_string(),
            enabled: false,
            checked: false,
        });
    } else {
        items.extend(
            devices
                .iter()
                .enumerate()
                .map(|(index, device)| ContextMenuItem {
                    id: format!("{prefix}:{index}"),
                    label: device.label.clone(),
                    enabled: true,
                    checked: selected_id == Some(device.id.as_str()),
                }),
        );
    }
    ContextMenuSection {
        id: id.to_string(),
        label: label.to_string(),
        items,
    }
}

fn selected_audio_device_patch(
    audio_devices: &AudioDeviceCatalog,
    item_id: &str,
    prefix: &str,
) -> Option<Option<String>> {
    let suffix = item_id.strip_prefix(prefix)?.strip_prefix(':')?;
    if suffix == "default" {
        return Some(None);
    }
    let index = suffix.parse::<usize>().ok()?;
    let devices = if prefix == "input" {
        &audio_devices.inputs
    } else {
        &audio_devices.outputs
    };
    devices.get(index).map(|device| Some(device.id.clone()))
}
