/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::env;
use std::fs;
use std::path::Path;

use servo_pdf::PdfDocument;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        println!(
            "Usage: cargo run -p servo-pdf --example render_pdf <path-to-pdf> [output-png] [page-num]"
        );
        return Ok(());
    }

    let input_path = &args[1];
    let output_path = args
        .get(2)
        .map(|s| s.as_str())
        .unwrap_or("rendered_page.png");
    let page_num: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);

    println!("Loading PDF: {}", input_path);
    let bytes = fs::read(input_path)?;
    let doc = PdfDocument::from_bytes(&bytes)?;

    println!("Total pages: {}", doc.page_count());
    let page = doc.page(page_num)?;
    let dims = page.dimensions();
    println!(
        "Page {} size: {} x {} pt",
        page_num, dims.width, dims.height
    );

    println!("Rendering at 2.0x scale (High DPI)...");
    let rendered = page.render(2.0)?;

    let paint_api::SerializableImageData::Raw(shared_mem) = rendered.data else {
        panic!("Expected Raw shared memory");
    };

    println!("Saving PNG to: {}", output_path);
    image::save_buffer(
        Path::new(output_path),
        &shared_mem,
        rendered.size.width,
        rendered.size.height,
        image::ExtendedColorType::Rgba8,
    )?;

    println!(
        "Success! Open {} to see the rendered PDF output.",
        output_path
    );
    Ok(())
}
