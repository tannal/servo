/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;
use std::fs;

use euclid::default::Size2D;
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
use crate::interpreter::{ContentStreamInterpreter, LoadedFont};

pub struct PdfPageRenderer {
    width: u16,
    height: u16,
    scale: f64,
    ctx: RenderContext,
    resources: Resources,
    pixmap: Pixmap,
    fonts: HashMap<Vec<u8>, LoadedFont>,
}

impl PdfPageRenderer {
    pub fn new(width: u16, height: u16, scale: f64) -> Self {
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


    fn load_page_fonts(&mut self, doc: &Document, page_id: ObjectId) -> Result<(), PdfError> {
        // lopdf returns (Option<&Dictionary>, Vec<ObjectId>)
        let (resources_opt, _) = doc.get_page_resources(page_id).map_err(PdfError::Parse)?;
        
        let Some(resources) = resources_opt else {
            return Ok(());
        };
    
        // System fallback font (DejaVuSans or LiberationSans on Linux)
        let fallback_font_bytes = std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf")
            .or_else(|_| std::fs::read("/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf"))
            .or_else(|_| std::fs::read("/usr/share/fonts/TTF/DejaVuSans.ttf"))
            .ok();
    
        if let Ok(fonts_dict) = resources.get(b"Font").and_then(|f| f.as_dict()) {
            for (font_name, font_ref) in fonts_dict.iter() {
                let mut loaded_font_data = None;
    
                if let Ok(font_deref) = doc.dereference(font_ref) {
                    if let Ok(font_dict) = font_deref.1.as_dict() {
                        if let Ok(desc_ref) = font_dict.get(b"FontDescriptor") {
                            if let Ok(desc_deref) = doc.dereference(desc_ref) {
                                if let Ok(desc) = desc_deref.1.as_dict() {
                                    let font_file_ref = desc.get(b"FontFile2").or_else(|_| desc.get(b"FontFile3"));
                                    if let Ok(ff_ref) = font_file_ref {
                                        if let Ok(ff_deref) = doc.dereference(ff_ref) {
                                            if let Ok(stream) = ff_deref.1.as_stream() {
                                                if let Ok(decompressed) = stream.decompressed_content() {
                                                    let blob = Blob::new(std::sync::Arc::new(decompressed));
                                                    loaded_font_data = Some(FontData::new(blob, 0));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
    
                // Fallback for unembedded Standard 14 fonts (Helvetica, Times, etc.)
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
