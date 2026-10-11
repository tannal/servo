/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use kurbo::{Affine, Point};
use lopdf::Object;
use lopdf::content::Operation;
use peniko::Color;
use std::collections::HashMap;

use servo_pdf::interpreter::ContentStreamInterpreter;

fn setup_test_interpreter() -> (
    vello_cpu::RenderContext,
    vello_cpu::Resources,
    HashMap<Vec<u8>, servo_pdf::interpreter::LoadedFont>,
) {
    let settings = vello_cpu::RenderSettings::default();
    let ctx = vello_cpu::RenderContext::new_with(100, 100, settings);
    let resources = vello_cpu::Resources::new();
    let fonts = HashMap::new();
    (ctx, resources, fonts)
}

#[test]
fn test_graphics_state_isolation() {
    let (mut ctx, mut resources, fonts) = setup_test_interpreter();
    let mut interp =
        ContentStreamInterpreter::new(&mut ctx, &mut resources, &fonts, Affine::IDENTITY);

    // Initial state: width = 1.0, color = Black
    interp.execute_operation(&Operation::new("q", vec![])); // Push
    interp.execute_operation(&Operation::new("w", vec![Object::Real(10.0)]));
    interp.execute_operation(&Operation::new(
        "rg",
        vec![Object::Real(0.0), Object::Real(1.0), Object::Real(0.0)],
    )); // Green
    interp.execute_operation(&Operation::new("Q", vec![])); // Pop

    // Must be back to initial state
    assert_eq!(interp.current_state().line_width, 1.0);
    assert_eq!(interp.current_state().fill_color, Color::BLACK);
}

#[test]
fn test_pdf_kerning_formula() {
    let (mut ctx, mut resources, fonts) = setup_test_interpreter();
    let mut interp =
        ContentStreamInterpreter::new(&mut ctx, &mut resources, &fonts, Affine::IDENTITY);

    interp.execute_operation(&Operation::new("BT", vec![]));
    interp.execute_operation(&Operation::new(
        "Tf",
        vec![Object::Name(b"F1".to_vec()), Object::Real(10.0)],
    )); // 10pt

    // ISO 32000: Kerning delta_x = -(adjustment / 1000) * font_size
    // If adjustment is -200 (moving cursor right by 2pt)
    interp.apply_kerning_adjustment(-200.0);

    let current_pos = interp.current_state().text_matrix * Point::ORIGIN;
    assert!((current_pos.x - 2.0).abs() < 1e-4);
}
