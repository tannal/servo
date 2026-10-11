/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;
use std::sync::Arc;

use kurbo::Affine;
use lopdf::content::Content;
use lopdf::{Document, ObjectId};
use paint_api::SerializableImageData;
use peniko::{Blob, FontData};
use servo_base::generic_channel::GenericSharedMemory;
use vello_cpu::{Pixmap, RenderContext, RenderSettings, Resources};
use webrender_api::units::DeviceIntSize;
use webrender_api::{ImageDescriptor, ImageDescriptorFlags, ImageFormat};

use crate::error::PdfError;
use crate::font_provider::{DefaultFontProvider, FontProvider};
use crate::interpreter::{ContentStreamInterpreter, LoadedFont};

pub struct PdfPageRenderer {
    width: u16,
    height: u16,
    scale: f64,
    ctx: RenderContext,
    resources: Resources,
    pixmap: Pixmap,
    fonts: HashMap<Vec<u8>, LoadedFont>,
    font_provider: Arc<dyn FontProvider>,
}

impl PdfPageRenderer {
    /// Create a renderer with the default system font provider.
    pub fn new(width: u16, height: u16, scale: f64) -> Self {
        Self::with_font_provider(width, height, scale, Arc::new(DefaultFontProvider))
    }

    /// Create a renderer with a custom font provider (e.g., from Servo's FontContext).
    pub fn with_font_provider(
        width: u16,
        height: u16,
        scale: f64,
        font_provider: Arc<dyn FontProvider>,
    ) -> Self {
        let settings = RenderSettings {
            num_threads: worker_thread_count(width, height),
            ..Default::default()
        };

        let ctx = RenderContext::new_with(width, height, settings);
        let resources = Resources::new();
        let pixmap = Pixmap::new(width, height);

        Self {
            width,
            height,
            scale,
            ctx,
            resources,
            pixmap,
            fonts: HashMap::new(),
            font_provider,
        }
    }

    pub fn render_page(&mut self, doc: &Document, page_id: ObjectId) -> Result<(), PdfError> {
        self.load_page_fonts(doc, page_id)?;

        let content_bytes = doc.get_page_content(page_id);
        let content = Content::decode(&content_bytes).map_err(PdfError::Parse)?;

        let base_transform = Affine::translate((0.0, self.height as f64))
            * Affine::scale_non_uniform(self.scale, -self.scale);

        let mut interpreter = ContentStreamInterpreter::new(
            &mut self.ctx,
            &mut self.resources,
            &self.fonts,
            base_transform,
        );
        interpreter.evaluate(&content);

        self.ctx.flush();
        self.ctx.render(&mut self.pixmap, &mut self.resources);

        Ok(())
    }

    /// Access the underlying pixmap (e.g. for CLI file export).
    pub fn pixmap(&self) -> &Pixmap {
        &self.pixmap
    }

    /// Extracts embedded font byte streams (/FontFile2 or /FontFile3),
    /// checking both direct /FontDescriptor and nested /DescendantFonts (for Type0 CID fonts).
    fn extract_font_file(doc: &Document, font_dict: &lopdf::Dictionary) -> Option<Vec<u8>> {
        // 1. Check direct /FontDescriptor
        let desc_obj = font_dict.get(b"FontDescriptor").ok().or_else(|| {
            // 2. Or inside /DescendantFonts array for Type0 / CID fonts
            font_dict
                .get(b"DescendantFonts")
                .ok()
                .and_then(|df| doc.dereference(df).ok())
                .and_then(|df_deref| df_deref.1.as_array().ok())
                .and_then(|arr| arr.first())
                .and_then(|cid_ref| doc.dereference(cid_ref).ok())
                .and_then(|cid_deref| cid_deref.1.as_dict().ok())
                .and_then(|cid_dict| cid_dict.get(b"FontDescriptor").ok())
        })?;

        let desc_deref = doc.dereference(desc_obj).ok()?;
        let desc = desc_deref.1.as_dict().ok()?;

        let font_file_ref = desc
            .get(b"FontFile2")
            .or_else(|_| desc.get(b"FontFile3"))
            .ok()?;
        let stream_deref = doc.dereference(font_file_ref).ok()?;
        let stream = stream_deref.1.as_stream().ok()?;

        stream.decompressed_content().ok()
    }

    fn load_page_fonts(&mut self, doc: &Document, page_id: ObjectId) -> Result<(), PdfError> {
        // lopdf returns (Option<&Dictionary>, Vec<ObjectId>)
        let (resources_opt, _) = doc.get_page_resources(page_id).map_err(PdfError::Parse)?;

        let Some(resources) = resources_opt else {
            return Ok(());
        };

        // System fallback font for unembedded standard fonts
        let fallback_font_bytes = self.font_provider.fallback_font();

        if let Ok(fonts_dict) = resources.get(b"Font").and_then(|f| f.as_dict()) {
            for (font_name, font_ref) in fonts_dict.iter() {
                let mut loaded_font_data = None;

                // 1. Try to load embedded font from PDF (TrueType, CFF, or Type0 DescendantFont)
                if let Ok(font_deref) = doc.dereference(font_ref) {
                    if let Ok(font_dict) = font_deref.1.as_dict() {
                        if let Some(font_bytes) = Self::extract_font_file(doc, font_dict) {
                            let blob = Blob::new(std::sync::Arc::new(font_bytes));
                            loaded_font_data = Some(FontData::new(blob, 0));
                        }
                    }
                }

                // 2. Fallback for unembedded Standard 14 fonts (Helvetica, Times, etc.)
                if loaded_font_data.is_none() {
                    if let Some(ref fb_bytes) = fallback_font_bytes {
                        let blob = Blob::new(std::sync::Arc::new(fb_bytes.clone()));
                        loaded_font_data = Some(FontData::new(blob, 0));
                    }
                }

                if let Some(font_data) = loaded_font_data {
                    self.fonts.insert(
                        font_name.clone(),
                        LoadedFont {
                            font_data,
                            default_width: 600.0,
                        },
                    );
                }
            }
        }

        Ok(())
    }

    pub fn into_webrender_image(&self) -> (ImageDescriptor, SerializableImageData) {
        let descriptor = ImageDescriptor {
            format: ImageFormat::RGBA8,
            size: DeviceIntSize::new(self.width as i32, self.height as i32),
            stride: None,
            offset: 0,
            flags: ImageDescriptorFlags::empty(),
        };

        let shared_memory = GenericSharedMemory::from_bytes(self.pixmap.data_as_u8_slice());
        (descriptor, SerializableImageData::Raw(shared_memory))
    }
}

fn worker_thread_count(width: u16, height: u16) -> u16 {
    const SMALL_CANVAS: u32 = 512 * 512;
    if (width as u32) * (height as u32) < SMALL_CANVAS {
        0
    } else {
        4
    }
}
