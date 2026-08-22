mod wasapi_utils;

use std::{
    collections::VecDeque,
    fs::File,
    io::Write as _,
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
};

use crate::wasapi_utils::{Direction, get_default_microphone, init_com_mta};

fn capture_thread(chunk_sender: SyncSender<Vec<u8>>) -> anyhow::Result<()> {
    let _com_mta_guard = init_com_mta()?;

    let microphone = get_default_microphone()?;

    let mut microphone_audio_client = microphone.get_audio_client()?;

    let microphone_mix_format = microphone_audio_client.get_mix_format()?;
    println!("Microphone mix format: {:?}", microphone_mix_format);

    microphone_audio_client.init_client_events_shared(
        &microphone_mix_format,
        &Direction::Capture,
        false,
    )?;

    let chunk_frame_count = 4096;
    let microphone_event_handle = microphone_audio_client.set_get_event_handle()?;
    let microphone_capture_client = microphone_audio_client.get_capture_client()?;
    let buffer_frame_count = microphone_audio_client.get_buffer_frame_count()?;
    let frame_bytes_count = microphone_mix_format.get_frame_bytes_count();
    let buffer_bytes_count = 100 * frame_bytes_count * (1024 + 2 * buffer_frame_count);
    let mut buffer: VecDeque<u8> = VecDeque::with_capacity(buffer_bytes_count);
    println!("Buffer bytes count: {}", buffer_bytes_count);
    let chunk_bytes_count = chunk_frame_count * frame_bytes_count;
    println!("Chunk bytes count: {}", chunk_bytes_count);

    // 开始捕获流
    microphone_audio_client.start_stream()?;
    loop {
        while buffer.len() >= chunk_bytes_count {
            let chunk = buffer.drain(..chunk_bytes_count).collect();
            chunk_sender.send(chunk)?;
        }
        microphone_capture_client.read_from_device_to_deque(&mut buffer)?;
        microphone_event_handle.wait_for_event(3000)?;
    }
}

fn main() -> anyhow::Result<()> {
    let (chunk_sender, chunk_receiver): (SyncSender<Vec<u8>>, Receiver<Vec<u8>>) =
        mpsc::sync_channel(2);
    let mut outfile = File::create("recorded.raw")?;
    // Capture
    let _handle = thread::spawn(move || {
        if let Err(err) = capture_thread(chunk_sender) {
            println!("Capture failed with error {}", err);
        }
    });
    loop {
        let chunk = chunk_receiver.recv()?;
        println!("Received chunk of size: {}", chunk.len());
        outfile.write_all(&chunk)?;
    }
}
