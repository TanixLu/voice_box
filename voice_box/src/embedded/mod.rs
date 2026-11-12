//! Files baked into the executable, exposed as raw data; the modules that use them
//! interpret them (`plugin` installs the DLL, `startup` fills the script template).
//! Preview inference borrows the same weights already stored inside the DLL.

use std::{ops::Deref, slice, sync::LazyLock};

use dfn3_rs::Model;

#[derive(Clone, Copy)]
#[repr(C, align(64))]
struct DllBlock([u8; 64]);

/// Keep the decompressed image aligned so inference can borrow its weights.
pub struct ApoDll {
    blocks: Box<[DllBlock]>,
    len: usize,
}

impl Deref for ApoDll {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: blocks are contiguous initialized bytes, with no internal
        // padding, and len excludes only the final block's unused bytes.
        unsafe { slice::from_raw_parts(self.blocks.as_ptr().cast(), self.len) }
    }
}

fn decompress(data: &[u8]) -> Vec<u8> {
    lz4_flex::block::decompress_size_prepended(data)
        .expect("UNREACHABLE: embedded data should be valid LZ4 data")
}

pub static APO_DLL: LazyLock<ApoDll> = LazyLock::new(|| {
    let compressed = include_bytes!(concat!(env!("OUT_DIR"), "/voice_box_apo.dll.lz4"));
    let (len, compressed) = lz4_flex::block::uncompressed_size(compressed)
        .expect("UNREACHABLE: embedded DLL should have an LZ4 size prefix");
    let mut blocks = vec![DllBlock([0; 64]); len.div_ceil(64)].into_boxed_slice();
    // SAFETY: blocks contain at least len initialized, exclusively owned bytes.
    let bytes = unsafe { slice::from_raw_parts_mut(blocks.as_mut_ptr().cast(), len) };
    let written = lz4_flex::block::decompress_into(compressed, bytes)
        .expect("UNREACHABLE: embedded DLL should be valid LZ4 data");
    assert_eq!(written, len, "embedded DLL size mismatch");
    ApoDll { blocks, len }
});

/// Locate weights in the DLL's file layout, without loading or executing it.
fn apo_model(dll: &'static [u8]) -> Model {
    fn read<const N: usize>(dll: &[u8], offset: usize) -> [u8; N] {
        dll.get(offset..)
            .and_then(|bytes| bytes.get(..N))
            .expect("Truncated APO DLL header")
            .try_into()
            .unwrap()
    }

    assert_eq!(&read::<2>(dll, 0), b"MZ", "Invalid APO DOS header");
    let pe = u32::from_le_bytes(read(dll, 0x3c)) as usize;
    assert_eq!(&read::<4>(dll, pe), b"PE\0\0", "Invalid APO PE header");
    let sections = u16::from_le_bytes(read(dll, pe + 6)) as usize;
    let optional_size = u16::from_le_bytes(read(dll, pe + 20)) as usize;
    let table = pe + 24 + optional_size;
    let mut model = None;
    for index in 0..sections {
        let header = table + index * 40;
        if &read::<8>(dll, header) != b".dfn3\0\0\0" {
            continue;
        }
        assert!(model.is_none(), "APO DLL contains multiple model sections");
        let size = u32::from_le_bytes(read(dll, header + 8)) as usize;
        let raw_size = u32::from_le_bytes(read(dll, header + 16)) as usize;
        let offset = u32::from_le_bytes(read(dll, header + 20)) as usize;
        let flags = u32::from_le_bytes(read(dll, header + 36));
        assert!(size <= raw_size, "Invalid APO model size");
        // IMAGE_SCN_MEM_READ, IMAGE_SCN_MEM_WRITE and IMAGE_SCN_MEM_DISCARDABLE.
        assert_eq!(
            flags & 0xc200_0000,
            0x4000_0000,
            "APO model must be read-only and non-discardable"
        );
        let bytes = dll
            .get(offset..)
            .and_then(|bytes| bytes.get(..size))
            .expect("Truncated APO model section");
        model = Some(
            Model::from_bytes(bytes)
                .expect("UNREACHABLE: bundled DLL should contain aligned DFN3 weights"),
        );
    }
    model.expect("APO DLL is missing the .dfn3 model section")
}

pub fn denoiser_model() -> Model {
    static MODEL: LazyLock<Model> = LazyLock::new(|| apo_model(&APO_DLL));
    *MODEL
}

pub static ICON_PNG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon.png"));

/// Contains `__VOICEBOX_*__` placeholders that `startup` replaces.
pub static STARTUP_SCRIPT: &str = include_str!("startup.ps1");

pub static UNINSTALL_CLEANUP_SCRIPT: &str = include_str!("uninstall_cleanup.ps1");

/// VoiceBox's own licenses followed by the third-party notices, assembled by `build.rs`.
pub static LICENSES: LazyLock<String> = LazyLock::new(|| {
    String::from_utf8(decompress(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/licenses.txt.lz4"
    ))))
    .expect("UNREACHABLE: embedded licenses should be UTF-8")
});
