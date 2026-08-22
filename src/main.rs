mod wasapi_utils;

use crate::wasapi_utils::{get_default_capture_device, get_default_render_device, init_com_mta};

fn main() -> anyhow::Result<()> {
    let _com_mta_guard = init_com_mta()?;

    let capture_device = get_default_capture_device()?;
    let render_device = get_default_render_device()?;

    let capture_client = capture_device.get_audio_client()?;
    let render_client = render_device.get_audio_client()?;

    let capture_format = capture_client.get_mix_format()?;
    let render_format = render_client.get_mix_format()?;

    dbg!(capture_format);
    dbg!(render_format);

    Ok(())
}
