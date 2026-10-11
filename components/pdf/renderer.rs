/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;

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
        let page_dict = doc.get_dictionary(page_id).map_err(PdfError::Parse)?;

        let Ok(resources_obj) = page_dict.get(b"Resources") else {
            return Ok(());
        };

        let Ok(resources_deref) = doc.dereference(resources_obj) else {
            return Ok(());
        };

        let Ok(fonts_dict_obj) = resources_deref.1.as_dict().and_then(|d| d.get(b"Font")) else {
            return Ok(());
        };

        let Ok(fonts_deref) = doc.dereference(fonts_dict_obj) else {
            return Ok(());
        };

        if let Ok(dict) = fonts_deref.1.as_dict() {
            for (font_name, font_ref) in dict.iter() {
                if let Ok(font_deref) = doc.dereference(font_ref) {
                    if let Ok(font_dict) = font_deref.1.as_dict() {
                        if let Ok(descriptor_obj) = font_dict.get(b"FontDescriptor") {
                            if let Ok(descriptor_deref) = doc.dereference(descriptor_obj) {
                                if let Ok(descriptor) = descriptor_deref.1.as_dict() {
                                    if let Ok(font_file_ref) = descriptor.get(b"FontFile2") {
                                        if let Ok(font_stream_deref) =
                                            doc.dereference(font_file_ref)
                                        {
                                            if let Ok(font_stream) = font_stream_deref.1.as_stream()
                                            {
                                                if let Ok(decompressed) =
                                                    font_stream.decompressed_content()
                                                {
                                                    let blob = Blob::new(std::sync::Arc::new(
                                                        decompressed,
                                                    ));
                                                    let font_data = FontData::new(blob, 0);
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
                                    }
                                }
                            }
                        }
                    }
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
