use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

type AudioObjectID = u32;
type AudioObjectPropertySelector = u32;
type AudioObjectPropertyScope = u32;
type AudioObjectPropertyElement = u32;
type OSStatus = i32;

const K_AUDIO_HARDWARE_NO_ERROR: OSStatus = 0;
const K_AUDIO_OBJECT_SYSTEM_OBJECT: AudioObjectID = 1;
const K_AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE: AudioObjectPropertySelector =
    u32::from_be_bytes(*b"dOut");
const K_AUDIO_DEVICE_PROPERTY_MUTE: AudioObjectPropertySelector = u32::from_be_bytes(*b"mute");
const K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT: AudioObjectPropertyScope = u32::from_be_bytes(*b"outp");
const K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL: AudioObjectPropertyScope = u32::from_be_bytes(*b"glob");
const K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN: AudioObjectPropertyElement = 0;

type AudioObjectPropertyListenerProc = unsafe extern "C" fn(
    AudioObjectID,
    u32,
    *const AudioObjectPropertyAddress,
    *mut c_void,
) -> OSStatus;

#[repr(C)]
struct AudioObjectPropertyAddress {
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
    element: AudioObjectPropertyElement,
}

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyData(
        object_id: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
        qualifier_data_size: u32,
        qualifier_data: *const c_void,
        data_size: *mut u32,
        data: *mut c_void,
    ) -> OSStatus;

    fn AudioObjectSetPropertyData(
        object_id: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
        qualifier_data_size: u32,
        qualifier_data: *const c_void,
        data_size: u32,
        data: *const c_void,
    ) -> OSStatus;

    fn AudioObjectAddPropertyListener(
        object_id: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
        listener: AudioObjectPropertyListenerProc,
        client_data: *mut c_void,
    ) -> OSStatus;

    fn AudioObjectRemovePropertyListener(
        object_id: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
        listener: AudioObjectPropertyListenerProc,
        client_data: *mut c_void,
    ) -> OSStatus;
}

fn default_output_device() -> Option<AudioObjectID> {
    let address = AudioObjectPropertyAddress {
        selector: K_AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE,
        scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
        element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };
    let mut device_id: AudioObjectID = 0;
    let mut size = std::mem::size_of::<AudioObjectID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            K_AUDIO_OBJECT_SYSTEM_OBJECT,
            &address,
            0,
            std::ptr::null(),
            &mut size,
            &mut device_id as *mut _ as *mut c_void,
        )
    };
    if status != K_AUDIO_HARDWARE_NO_ERROR || device_id == 0 {
        tracing::warn!(status, "Failed to get default output audio device");
        return None;
    }
    Some(device_id)
}

fn get_mute(device_id: AudioObjectID) -> Option<bool> {
    let address = AudioObjectPropertyAddress {
        selector: K_AUDIO_DEVICE_PROPERTY_MUTE,
        scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT,
        element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };
    let mut muted: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            &address,
            0,
            std::ptr::null(),
            &mut size,
            &mut muted as *mut _ as *mut c_void,
        )
    };
    if status != K_AUDIO_HARDWARE_NO_ERROR {
        tracing::debug!(status, device_id, "Failed to read mute state");
        return None;
    }
    Some(muted != 0)
}

fn set_mute(device_id: AudioObjectID, mute: bool) -> bool {
    let address = AudioObjectPropertyAddress {
        selector: K_AUDIO_DEVICE_PROPERTY_MUTE,
        scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT,
        element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };
    let value: u32 = if mute { 1 } else { 0 };
    let status = unsafe {
        AudioObjectSetPropertyData(
            device_id,
            &address,
            0,
            std::ptr::null(),
            std::mem::size_of::<u32>() as u32,
            &value as *const _ as *const c_void,
        )
    };
    if status != K_AUDIO_HARDWARE_NO_ERROR {
        tracing::warn!(status, device_id, mute, "Failed to set mute state");
        return false;
    }
    true
}

unsafe extern "C" fn mute_listener_callback(
    object_id: AudioObjectID,
    _num_addresses: u32,
    _addresses: *const AudioObjectPropertyAddress,
    client_data: *mut c_void,
) -> OSStatus {
    let enforce = &*(client_data as *const AtomicBool);
    if !enforce.load(Ordering::Relaxed) {
        return K_AUDIO_HARDWARE_NO_ERROR;
    }
    if let Some(false) = get_mute(object_id) {
        tracing::info!("External unmute detected — re-muting local audio");
        set_mute(object_id, true);
    }
    K_AUDIO_HARDWARE_NO_ERROR
}

fn mute_address() -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        selector: K_AUDIO_DEVICE_PROPERTY_MUTE,
        scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT,
        element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    }
}

/// Manages muting the local Mac speaker while audio is being streamed over RDP.
///
/// On `mute_local()`, records whether the device was already muted (to avoid
/// unmuting a device the user intentionally muted) and sets mute. A CoreAudio
/// property listener re-mutes the device if something else (e.g. a forwarded
/// volume key) unmutes it while the guard is active. On `restore_local()` or
/// `Drop`, the listener is removed and prior mute state is restored.
pub(crate) struct LocalMuteGuard {
    device_id: AudioObjectID,
    was_already_muted: bool,
    // Shared with the CoreAudio listener callback; when true the callback
    // re-mutes on any external unmute. Cleared before restoring so the
    // callback doesn't fight the restore.
    enforce_mute: Arc<AtomicBool>,
}

impl LocalMuteGuard {
    pub(crate) fn mute_local() -> Option<Self> {
        let device_id = default_output_device()?;
        let was_already_muted = get_mute(device_id).unwrap_or(false);

        if was_already_muted {
            tracing::info!("Local audio already muted — will leave unchanged on restore");
        } else if set_mute(device_id, true) {
            tracing::info!("Muted local audio output for RDP audio redirection");
        } else {
            return None;
        }

        let enforce_mute = Arc::new(AtomicBool::new(true));

        let addr = mute_address();
        let status = unsafe {
            AudioObjectAddPropertyListener(
                device_id,
                &addr,
                mute_listener_callback,
                Arc::as_ptr(&enforce_mute) as *mut c_void,
            )
        };
        if status != K_AUDIO_HARDWARE_NO_ERROR {
            tracing::warn!(status, "Failed to install mute property listener");
        }

        Some(Self {
            device_id,
            was_already_muted,
            enforce_mute,
        })
    }

    pub(crate) fn restore_local(&self) {
        if !self.enforce_mute.swap(false, Ordering::Relaxed) {
            return;
        }

        let addr = mute_address();
        let status = unsafe {
            AudioObjectRemovePropertyListener(
                self.device_id,
                &addr,
                mute_listener_callback,
                Arc::as_ptr(&self.enforce_mute) as *mut c_void,
            )
        };
        if status != K_AUDIO_HARDWARE_NO_ERROR {
            tracing::warn!(status, "Failed to remove mute property listener");
        }

        if self.was_already_muted {
            tracing::info!("Local audio was already muted before RDP — leaving muted");
            return;
        }
        if set_mute(self.device_id, false) {
            tracing::info!("Restored local audio output (unmuted)");
        }
    }
}

impl Drop for LocalMuteGuard {
    fn drop(&mut self) {
        self.restore_local();
    }
}
