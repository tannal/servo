/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

pub mod error;
pub mod interpreter;
pub mod renderer;

use std::sync::Arc;

pub use error::PdfError;
use euclid::default::Size2D;
pub use renderer::PdfPageRenderer;

use paint_api::SerializableImageData;
use webrender_api::ImageDescriptor;

#[derive(Clone)]
pub struct PdfDocument {
    inner: Arc<lopdf::Document>,
}

impl PdfDocument {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PdfError> {
        let doc = lopdf::Document::load_mem(bytes).map_err(PdfError::Parse)?;
        Ok(Self {
            inner: Arc::new(doc),
        })
    }

    pub fn page_count(&self) -> usize {
        self.inner.get_pages().len()
    }

    pub fn page(&self, page_number: u32) -> Result<PdfPage, PdfError> {
        let pages = self.inner.get_pages();
        let page_id = pages
            .get(&page_number)
            .copied()
            .ok_or(PdfError::PageNotFound(page_number))?;

        let media_box = self.get_page_media_box(page_id)?;

        Ok(PdfPage {
            document: self.clone(),
            page_id,
            page_number,
            width_pts: media_box.width,
            height_pts: media_box.height,
        })
    }

    fn get_page_media_box(&self, page_id: lopdf::ObjectId) -> Result<Size2D<f64>, PdfError> {
        let page_dict = self
            .inner
            .get_dictionary(page_id)
            .map_err(PdfError::Parse)?;
        let media_box_obj = page_dict
            .get(b"MediaBox")
            .map_err(|_| PdfError::MissingAttribute("MediaBox"))?;

        let array = media_box_obj.as_array().map_err(PdfError::Parse)?;
        if array.len() < 4 {
            return Err(PdfError::InvalidFormat(
                "MediaBox array must have at least 4 numbers",
            ));
        }

        let x1 = to_f64(&array[0]);
        let y1 = to_f64(&array[1]);
        let x2 = to_f64(&array[2]);
        let y2 = to_f64(&array[3]);

        let width = (x2 - x1).abs();
        let height = (y2 - y1).abs();

        Ok(Size2D::new(width, height))
    }
}

pub struct PdfPage {
    document: PdfDocument,
    page_id: lopdf::ObjectId,
    page_number: u32,
    pub width_pts: f64,
    pub height_pts: f64,
}

impl PdfPage {
    pub fn number(&self) -> u32 {
        self.page_number
    }

    pub fn dimensions(&self) -> Size2D<f64> {
        Size2D::new(self.width_pts, self.height_pts)
    }

    pub fn render(&self, scale: f64) -> Result<RenderedPage, PdfError> {
        let pixel_width = (self.width_pts * scale).ceil() as u32;
        let pixel_height = (self.height_pts * scale).ceil() as u32;

        if pixel_width == 0 || pixel_height == 0 {
            return Err(PdfError::InvalidDimensions);
        }

        let w_u16: u16 = pixel_width.try_into().unwrap_or(u16::MAX);
        let h_u16: u16 = pixel_height.try_into().unwrap_or(u16::MAX);

        let mut renderer = PdfPageRenderer::new(w_u16, h_u16, scale);
        renderer.render_page(&self.document.inner, self.page_id)?;

        let (descriptor, shared_memory) = renderer.into_webrender_image();

        Ok(RenderedPage {
            size: Size2D::new(pixel_width, pixel_height),
            scale,
            descriptor,
            data: shared_memory,
        })
    }
}

pub struct RenderedPage {
    pub size: Size2D<u32>,
    pub scale: f64,
    pub descriptor: ImageDescriptor,
    pub data: SerializableImageData,
}

impl RenderedPage {
    pub fn into_webrender_payload(self) -> (ImageDescriptor, SerializableImageData) {
        (self.descriptor, self.data)
    }
}

fn to_f64(obj: &lopdf::Object) -> f64 {
    match obj {
        lopdf::Object::Real(val) => (*val) as f64,
        lopdf::Object::Integer(val) => *val as f64,
        _ => 0.0,
    }
}
