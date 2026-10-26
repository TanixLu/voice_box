//! Registration identity and properties shared by the APO and setup checks.
use windows::Win32::Media::Audio::Apo::{
    APO_FLAG, APO_FLAG_BITSPERSAMPLE_MUST_MATCH, APO_FLAG_FRAMESPERSECOND_MUST_MATCH,
    APO_FLAG_INPLACE, APO_FLAG_SAMPLESPERFRAME_MUST_MATCH, APO_REG_PROPERTIES,
    IAudioProcessingObject,
};
use windows_core::{GUID, Interface};

pub const CLSID: GUID = GUID::from_u128(0xae672075_b244_4417_b5a5_fbd45b7f0268);
pub const NAME: &str = "VoiceBox APO";
pub const COPYRIGHT: &str = "Copyright (C) 2026 Tanix";

pub fn registration_properties() -> APO_REG_PROPERTIES {
    let mut properties = APO_REG_PROPERTIES {
        clsid: CLSID,
        Flags: APO_FLAG(
            APO_FLAG_INPLACE.0
                | APO_FLAG_SAMPLESPERFRAME_MUST_MATCH.0
                | APO_FLAG_FRAMESPERSECOND_MUST_MATCH.0
                | APO_FLAG_BITSPERSAMPLE_MUST_MATCH.0,
        ),
        u32MajorVersion: 1,
        u32MinorVersion: 0,
        u32MinInputConnections: 1,
        u32MaxInputConnections: 1,
        u32MinOutputConnections: 1,
        u32MaxOutputConnections: 1,
        u32MaxInstances: u32::MAX,
        u32NumAPOInterfaces: 1,
        iidAPOInterfaceList: [IAudioProcessingObject::IID],
        ..Default::default()
    };
    write_wide(&mut properties.szFriendlyName, NAME);
    write_wide(&mut properties.szCopyrightInfo, COPYRIGHT);
    properties
}

fn write_wide(destination: &mut [u16], text: &str) {
    for (slot, value) in destination.iter_mut().zip(text.encode_utf16()) {
        *slot = value;
    }
}
