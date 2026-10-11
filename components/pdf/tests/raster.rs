/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, Stream, dictionary};
use servo_pdf::PdfDocument;

fn generate_synthetic_page(ops: Vec<Operation>) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let content = Content { operations: ops };
    let content_stream = Stream::new(dictionary! {}, content.encode().unwrap());
    let content_id = doc.add_object(content_stream);

    let page_id = doc.add_object(dictionary! {
         "Type" => "Page",
         "MediaBox" => vec![Object::Integer(0), Object::Integer(0), Object::Integer(50), Object::Integer(50)],
         "Contents" => content_id,
     });

    let pages_id = doc.add_object(dictionary! {
        "Type" => "Pages",
        "Kids" => vec![page_id.into()],
        "Count" => 1,
    });

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });

    doc.trailer.set("Root", catalog_id);
    let mut buf = Vec::new();
    doc.save_to(&mut buf).unwrap();
    buf
}

#[test]
fn test_render_filled_blue_box() {
    // PDF commands: blue color (0 0 1 rg), fill rect 0..50
    let ops = vec![
        Operation::new(
            "rg",
            vec![Object::Real(0.0), Object::Real(0.0), Object::Real(1.0)],
        ),
        Operation::new(
            "re",
            vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(50.0),
                Object::Real(50.0),
            ],
        ),
        Operation::new("f", vec![]),
    ];
    let pdf_bytes = generate_synthetic_page(ops);

    let doc = PdfDocument::from_bytes(&pdf_bytes).expect("Valid PDF");
    let page = doc.page(1).expect("Page 1 exists");
    let rendered = page.render(1.0).expect("Render success");

    let (descriptor, data) = rendered.into_webrender_payload();
    assert_eq!(descriptor.size.width, 50);
    assert_eq!(descriptor.size.height, 50);

    let paint_api::SerializableImageData::Raw(shared_mem) = data else {
        panic!("Must be Raw shared memory");
    };
    let pixels: &[u8] = &shared_mem;

    // Center pixel at (25, 25) must be Blue: [0, 0, 255, 255]
    let center = (25 * 50 + 25) * 4;
    assert_eq!(pixels[center], 0); // R
    assert_eq!(pixels[center + 1], 0); // G
    assert_eq!(pixels[center + 2], 255); // B
    assert_eq!(pixels[center + 3], 255); // A
}
