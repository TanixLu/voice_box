use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{Context as _, ensure};
use egui::epaint::text::{Blob, FallbackRequest, FontProvider};
use egui::{FontData, FontFamily, FontInsert, FontTweak};
use windows::{
    Win32::{
        Globalization::GetUserDefaultLocaleName,
        Graphics::DirectWrite::{
            DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_WEIGHT_REGULAR, DWRITE_READING_DIRECTION,
            DWRITE_READING_DIRECTION_LEFT_TO_RIGHT, DWriteCreateFactory, IDWriteFactory2,
            IDWriteFontCollection, IDWriteFontFallback, IDWriteNumberSubstitution,
            IDWriteTextAnalysisSource, IDWriteTextAnalysisSource_Impl,
        },
        System::SystemServices::LOCALE_NAME_MAX_LENGTH,
    },
    core::{Interface, OutRef, implement, w},
};

pub(super) fn install(ctx: &egui::Context) {
    match DirectWriteFonts::new() {
        Ok(provider) => ctx.add_font_provider(Arc::new(provider)),
        Err(error) => tracing::warn!("Failed to initialize DirectWrite font fallback: {error:#}"),
    }
}

struct DirectWriteFonts {
    fallback: IDWriteFontFallback,
    locale: [u16; LOCALE_NAME_MAX_LENGTH as usize],
    files: Mutex<HashMap<String, Blob>>,
}

impl DirectWriteFonts {
    fn new() -> anyhow::Result<Self> {
        let factory: IDWriteFactory2 = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let mut locale = [0; LOCALE_NAME_MAX_LENGTH as usize];
        if unsafe { GetUserDefaultLocaleName(&mut locale) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self {
            fallback: unsafe { factory.GetSystemFontFallback()? },
            locale,
            files: Mutex::new(HashMap::new()),
        })
    }

    fn find_font(&self, request: &FallbackRequest<'_>) -> anyhow::Result<Option<FontInsert>> {
        if request.cluster.is_empty() {
            return Ok(None);
        }
        let text: Vec<u16> = request.cluster.encode_utf16().collect();
        let length = text.len().try_into()?;
        let source: IDWriteTextAnalysisSource = TextSource {
            text,
            locale: self.locale,
        }
        .into();
        let family = match request.family {
            FontFamily::Monospace => w!("Consolas"),
            _ => w!("Segoe UI"),
        };
        let mut mapped_length = 0;
        let mut font = None;
        let mut scale = 1.0;
        unsafe {
            // Query the actual cluster; fontique 0.11 queries a fixed sample for its script.
            self.fallback.MapCharacters(
                &source,
                0,
                length,
                None::<&IDWriteFontCollection>,
                family,
                DWRITE_FONT_WEIGHT_REGULAR,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                &mut mapped_length,
                &mut font,
                &mut scale,
            )?;
            let Some(font) = font else { return Ok(None) };
            let face = font.CreateFontFace()?;
            let mut files = [None];
            face.GetFiles(&mut 1, Some(files.as_mut_ptr()))?;
            let file = files[0]
                .take()
                .context("DirectWrite returned no font file")?;
            let mut key = std::ptr::null_mut();
            let mut key_size = 0;
            file.GetReferenceKey(&mut key, &mut key_size)?;
            let loader = file.GetLoader()?;
            let key_bytes = if key_size == 0 {
                &[]
            } else {
                ensure!(!key.is_null(), "DirectWrite returned a null font file key");
                std::slice::from_raw_parts(key.cast::<u8>(), key_size as usize)
            };
            let file_key = format!("directwrite:{:p}:{key_bytes:02x?}", loader.as_raw());
            let mut cache = self.files.lock().unwrap_or_else(|error| error.into_inner());
            let blob = if let Some(blob) = cache.get(&file_key) {
                Arc::clone(blob)
            } else {
                let stream = loader.CreateStreamFromKey(key, key_size)?;
                let size = usize::try_from(stream.GetFileSize()?)?;
                ensure!(
                    size > 0 && size <= isize::MAX as usize,
                    "Invalid font file size"
                );
                let mut fragment = std::ptr::null_mut();
                let mut context = std::ptr::null_mut();
                stream.ReadFileFragment(&mut fragment, 0, size as u64, &mut context)?;
                let bytes = if fragment.is_null() {
                    None
                } else {
                    Some(std::slice::from_raw_parts(fragment.cast::<u8>(), size).to_vec())
                };
                stream.ReleaseFileFragment(context);
                let blob: Blob =
                    Arc::new(bytes.context("DirectWrite returned a null font fragment")?);
                cache.insert(file_key.clone(), Arc::clone(&blob));
                blob
            };
            Ok(Some(FontInsert::new(
                &format!("{file_key}:{}:{scale}", face.GetIndex()),
                FontData::from_blob(blob, face.GetIndex()).tweak(FontTweak {
                    scale,
                    ..Default::default()
                }),
                Vec::new(),
            )))
        }
    }
}

impl FontProvider for DirectWriteFonts {
    fn fonts_for_family(&self, family: &FontFamily) -> Vec<FontInsert> {
        // Seed the family before egui requests individual missing characters.
        self.font_for(&FallbackRequest {
            cluster: "A",
            family,
        })
        .into_iter()
        .collect()
    }

    fn font_for(&self, request: &FallbackRequest<'_>) -> Option<FontInsert> {
        match self.find_font(request) {
            Ok(font) => font,
            Err(error) => {
                tracing::warn!(
                    cluster = request.cluster,
                    "DirectWrite font fallback failed: {error:#}"
                );
                None
            }
        }
    }
}

#[implement(IDWriteTextAnalysisSource)]
struct TextSource {
    text: Vec<u16>,
    locale: [u16; LOCALE_NAME_MAX_LENGTH as usize],
}

impl IDWriteTextAnalysisSource_Impl for TextSource_Impl {
    fn GetTextAtPosition(
        &self,
        position: u32,
        text: *mut *mut u16,
        length: *mut u32,
    ) -> windows::core::Result<()> {
        let remaining = self.text.get(position as usize..).unwrap_or_default();
        unsafe {
            *length = remaining.len() as u32;
            *text = if remaining.is_empty() {
                std::ptr::null_mut()
            } else {
                remaining.as_ptr().cast_mut()
            };
        }
        Ok(())
    }

    fn GetTextBeforePosition(
        &self,
        position: u32,
        text: *mut *mut u16,
        length: *mut u32,
    ) -> windows::core::Result<()> {
        let before = self.text.get(..position as usize).unwrap_or_default();
        unsafe {
            *length = before.len() as u32;
            *text = if before.is_empty() {
                std::ptr::null_mut()
            } else {
                before.as_ptr().cast_mut()
            };
        }
        Ok(())
    }

    fn GetParagraphReadingDirection(&self) -> DWRITE_READING_DIRECTION {
        DWRITE_READING_DIRECTION_LEFT_TO_RIGHT
    }

    fn GetLocaleName(
        &self,
        position: u32,
        length: *mut u32,
        locale: *mut *mut u16,
    ) -> windows::core::Result<()> {
        unsafe {
            *length = (self.text.len() as u32).saturating_sub(position);
            *locale = self.locale.as_ptr().cast_mut();
        }
        Ok(())
    }

    fn GetNumberSubstitution(
        &self,
        position: u32,
        length: *mut u32,
        substitution: OutRef<'_, IDWriteNumberSubstitution>,
    ) -> windows::core::Result<()> {
        unsafe { *length = (self.text.len() as u32).saturating_sub(position) };
        substitution.write(None)
    }
}
