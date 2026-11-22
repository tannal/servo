/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#![expect(unsafe_code)]

use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::sync::LazyLock;
use std::{char, ptr};

use app_units::Au;
use euclid::default::Point2D;
// Eventually we would like the shaper to be pluggable, as many operating systems have their own
// shapers. For now, however, HarfBuzz is a hard dependency.
use harfbuzz_sys::{
    HB_BUFFER_CLUSTER_LEVEL_MONOTONE_CHARACTERS, HB_DIRECTION_LTR, HB_DIRECTION_RTL,
    HB_DIRECTION_TTB, HB_MEMORY_MODE_READONLY, hb_blob_create, hb_blob_t, hb_bool_t,
    hb_buffer_add_utf8, hb_buffer_create, hb_buffer_destroy, hb_buffer_get_glyph_infos,
    hb_buffer_get_glyph_positions, hb_buffer_set_cluster_level, hb_buffer_set_direction,
    hb_buffer_set_language, hb_buffer_set_script, hb_buffer_t, hb_codepoint_t,
    hb_face_create_for_tables, hb_face_destroy, hb_face_t, hb_feature_t, hb_font_create,
    hb_font_create_sub_font, hb_font_destroy, hb_font_funcs_create,
    hb_font_funcs_set_glyph_h_advance_func, hb_font_funcs_set_nominal_glyph_func, hb_font_funcs_t,
    hb_font_get_glyph_extents, hb_font_get_glyph_h_advance, hb_font_get_nominal_glyph,
    hb_font_set_funcs, hb_font_set_ppem, hb_font_set_scale, hb_font_set_variations, hb_font_t,
    hb_glyph_extents_t, hb_glyph_info_t, hb_glyph_position_t, hb_language_from_string,
    hb_ot_tag_to_language, hb_position_t, hb_script_from_iso15924_tag, hb_shape, hb_tag_t,
    hb_variation_t,
};
use harfbuzz_sys::{
    HB_OT_MATH_CONSTANT_ACCENT_BASE_HEIGHT, HB_OT_MATH_CONSTANT_AXIS_HEIGHT,
    HB_OT_MATH_CONSTANT_DELIMITED_SUB_FORMULA_MIN_HEIGHT,
    HB_OT_MATH_CONSTANT_DISPLAY_OPERATOR_MIN_HEIGHT,
    HB_OT_MATH_CONSTANT_FLATTENED_ACCENT_BASE_HEIGHT,
    HB_OT_MATH_CONSTANT_FRACTION_DENOM_DISPLAY_STYLE_GAP_MIN,
    HB_OT_MATH_CONSTANT_FRACTION_DENOMINATOR_DISPLAY_STYLE_SHIFT_DOWN,
    HB_OT_MATH_CONSTANT_FRACTION_DENOMINATOR_GAP_MIN,
    HB_OT_MATH_CONSTANT_FRACTION_DENOMINATOR_SHIFT_DOWN,
    HB_OT_MATH_CONSTANT_FRACTION_NUM_DISPLAY_STYLE_GAP_MIN,
    HB_OT_MATH_CONSTANT_FRACTION_NUMERATOR_DISPLAY_STYLE_SHIFT_UP,
    HB_OT_MATH_CONSTANT_FRACTION_NUMERATOR_GAP_MIN,
    HB_OT_MATH_CONSTANT_FRACTION_NUMERATOR_SHIFT_UP, HB_OT_MATH_CONSTANT_FRACTION_RULE_THICKNESS,
    HB_OT_MATH_CONSTANT_LOWER_LIMIT_BASELINE_DROP_MIN, HB_OT_MATH_CONSTANT_LOWER_LIMIT_GAP_MIN,
    HB_OT_MATH_CONSTANT_OVERBAR_EXTRA_ASCENDER, HB_OT_MATH_CONSTANT_OVERBAR_RULE_THICKNESS,
    HB_OT_MATH_CONSTANT_OVERBAR_VERTICAL_GAP,
    HB_OT_MATH_CONSTANT_RADICAL_DEGREE_BOTTOM_RAISE_PERCENT,
    HB_OT_MATH_CONSTANT_RADICAL_DISPLAY_STYLE_VERTICAL_GAP,
    HB_OT_MATH_CONSTANT_RADICAL_EXTRA_ASCENDER, HB_OT_MATH_CONSTANT_RADICAL_KERN_AFTER_DEGREE,
    HB_OT_MATH_CONSTANT_RADICAL_KERN_BEFORE_DEGREE, HB_OT_MATH_CONSTANT_RADICAL_RULE_THICKNESS,
    HB_OT_MATH_CONSTANT_RADICAL_VERTICAL_GAP, HB_OT_MATH_CONSTANT_SCRIPT_PERCENT_SCALE_DOWN,
    HB_OT_MATH_CONSTANT_SCRIPT_SCRIPT_PERCENT_SCALE_DOWN, HB_OT_MATH_CONSTANT_SPACE_AFTER_SCRIPT,
    HB_OT_MATH_CONSTANT_STACK_BOTTOM_DISPLAY_STYLE_SHIFT_DOWN,
    HB_OT_MATH_CONSTANT_STACK_BOTTOM_SHIFT_DOWN, HB_OT_MATH_CONSTANT_STACK_DISPLAY_STYLE_GAP_MIN,
    HB_OT_MATH_CONSTANT_STACK_GAP_MIN, HB_OT_MATH_CONSTANT_STACK_TOP_DISPLAY_STYLE_SHIFT_UP,
    HB_OT_MATH_CONSTANT_STACK_TOP_SHIFT_UP, HB_OT_MATH_CONSTANT_STRETCH_STACK_BOTTOM_SHIFT_DOWN,
    HB_OT_MATH_CONSTANT_STRETCH_STACK_GAP_ABOVE_MIN,
    HB_OT_MATH_CONSTANT_STRETCH_STACK_GAP_BELOW_MIN,
    HB_OT_MATH_CONSTANT_STRETCH_STACK_TOP_SHIFT_UP,
    HB_OT_MATH_CONSTANT_SUB_SUPERSCRIPT_GAP_MIN, HB_OT_MATH_CONSTANT_SUBSCRIPT_BASELINE_DROP_MIN,
    HB_OT_MATH_CONSTANT_SUBSCRIPT_SHIFT_DOWN, HB_OT_MATH_CONSTANT_SUBSCRIPT_TOP_MAX,
    HB_OT_MATH_CONSTANT_SUPERSCRIPT_BASELINE_DROP_MAX, HB_OT_MATH_CONSTANT_SUPERSCRIPT_BOTTOM_MIN,
    HB_OT_MATH_CONSTANT_SUPERSCRIPT_BOTTOM_MAX_WITH_SUBSCRIPT,
    HB_OT_MATH_CONSTANT_SUPERSCRIPT_SHIFT_UP, HB_OT_MATH_CONSTANT_SUPERSCRIPT_SHIFT_UP_CRAMPED,
    HB_OT_MATH_CONSTANT_UNDERBAR_EXTRA_DESCENDER, HB_OT_MATH_CONSTANT_UNDERBAR_RULE_THICKNESS,
    HB_OT_MATH_CONSTANT_UNDERBAR_VERTICAL_GAP, HB_OT_MATH_CONSTANT_UPPER_LIMIT_BASELINE_RISE_MIN,
    HB_OT_MATH_CONSTANT_UPPER_LIMIT_GAP_MIN, HB_OT_MATH_GLYPH_PART_FLAG_EXTENDER,
    hb_ot_math_get_constant, hb_ot_math_get_glyph_assembly,
    hb_ot_math_get_glyph_italics_correction, hb_ot_math_get_glyph_top_accent_attachment,
    hb_ot_math_get_glyph_variants, hb_ot_math_get_min_connector_overlap, hb_ot_math_glyph_part_t,
    hb_ot_math_glyph_variant_t, hb_ot_math_has_data,
};

use num_traits::Zero;
use read_fonts::types::Tag;

use super::{GlyphShapingResult, ShapedGlyph, unicode_script_to_iso15924_tag};
use crate::font::FontTableMethods;
use crate::platform::font::FontTable;
use crate::{
    Font, GlyphId, MathConstants, MathFontScales, MathGlyphExtents, MathStretchDirection,
    PositionedMathGlyph, ShapedText, ShapingFlags, ShapingOptions, StretchedMathGlyph,
    fixed_to_float, float_to_fixed,
};

const HB_OT_TAG_DEFAULT_LANGUAGE: hb_tag_t = u32::from_be_bytes(Tag::new(b"dflt").to_be_bytes());

pub(crate) struct HarfbuzzGlyphShapingResult {
    count: usize,
    buffer: *mut hb_buffer_t,
    glyph_infos: *mut hb_glyph_info_t,
    pos_infos: *mut hb_glyph_position_t,
}

impl HarfbuzzGlyphShapingResult {
    /// Create a new [`HarfbuzzGlyphShapingResult`] from the given HarfBuzz buffer.
    ///
    /// # Safety
    ///
    /// - Passing an invalid buffer pointer to this function results in undefined behavior.
    /// - This function takes ownership of the buffer and the HarfbuzzGlyphShapingResult destroys the buffer when dropped
    ///   so the pointer must an owned pointer and must not be used after being passed to this function
    unsafe fn new(buffer: *mut hb_buffer_t) -> HarfbuzzGlyphShapingResult {
        let mut glyph_count = 0;
        let glyph_infos = unsafe { hb_buffer_get_glyph_infos(buffer, &mut glyph_count) };
        assert!(!glyph_infos.is_null());
        let mut pos_count = 0;
        let pos_infos = unsafe { hb_buffer_get_glyph_positions(buffer, &mut pos_count) };
        assert!(!pos_infos.is_null());
        assert_eq!(glyph_count, pos_count);

        HarfbuzzGlyphShapingResult {
            count: glyph_count as usize,
            buffer,
            glyph_infos,
            pos_infos,
        }
    }
}

impl Drop for HarfbuzzGlyphShapingResult {
    fn drop(&mut self) {
        unsafe { hb_buffer_destroy(self.buffer) }
    }
}

struct ShapedGlyphIterator<'a> {
    shaped_glyph_data: &'a HarfbuzzGlyphShapingResult,
    current_glyph_offset: usize,
    y_position: Au,
}

impl<'a> Iterator for ShapedGlyphIterator<'a> {
    type Item = ShapedGlyph;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_glyph_offset >= self.shaped_glyph_data.count {
            return None;
        }

        unsafe {
            let glyph_info_i = self
                .shaped_glyph_data
                .glyph_infos
                .add(self.current_glyph_offset);
            let pos_info_i = self
                .shaped_glyph_data
                .pos_infos
                .add(self.current_glyph_offset);
            let x_offset = Shaper::fixed_to_float((*pos_info_i).x_offset);
            let y_offset = Shaper::fixed_to_float((*pos_info_i).y_offset);
            let x_advance = Shaper::fixed_to_float((*pos_info_i).x_advance);
            let y_advance = Shaper::fixed_to_float((*pos_info_i).y_advance);

            let x_offset = Au::from_f64_px(x_offset);
            let y_offset = Au::from_f64_px(y_offset);
            let x_advance = Au::from_f64_px(x_advance);
            let y_advance = Au::from_f64_px(y_advance);

            let offset = if x_offset.is_zero() && y_offset.is_zero() && y_advance.is_zero() {
                None
            } else {
                // adjust the pen..
                if y_advance > Au::zero() {
                    self.y_position -= y_advance;
                }

                Some(Point2D::new(x_offset, self.y_position - y_offset))
            };

            self.current_glyph_offset += 1;
            Some(ShapedGlyph {
                glyph_id: (*glyph_info_i).codepoint as GlyphId,
                string_byte_offset: (*glyph_info_i).cluster as usize,
                advance: x_advance,
                offset,
            })
        }
    }
}

impl GlyphShapingResult for HarfbuzzGlyphShapingResult {
    #[inline]
    fn len(&self) -> usize {
        self.count
    }

    fn iter(&self) -> impl Iterator<Item = ShapedGlyph> {
        ShapedGlyphIterator {
            shaped_glyph_data: self,
            current_glyph_offset: 0,
            y_position: Au::zero(),
        }
    }

    fn is_rtl(&self) -> bool {
        if self.count == 0 {
            return false;
        }
        // SAFETY: We assume self.glyph_infos is a valid non-zero ptr
        // to an array of self.count items. We checked self.count != 0
        // and hence both calculated pointers are within the bounds.
        unsafe {
            let first_glyph_info = self.glyph_infos.add(0);
            let last_glyph_info = self.glyph_infos.add(self.count - 1);
            (*last_glyph_info).cluster < (*first_glyph_info).cluster
        }
    }
}

#[derive(Debug)]
pub(crate) struct Shaper {
    hb_face: *mut hb_face_t,
    hb_font: *mut hb_font_t,
}

// The HarfBuzz API is thread safe as well as our `Font`, so we can make the data
// structures here as thread-safe as well. This doesn't seem to be documented,
// but was expressed as one of the original goals of the HarfBuzz API.
unsafe impl Sync for Shaper {}
unsafe impl Send for Shaper {}

impl Drop for Shaper {
    fn drop(&mut self) {
        unsafe {
            assert!(!self.hb_face.is_null());
            hb_face_destroy(self.hb_face);

            assert!(!self.hb_font.is_null());
            hb_font_destroy(self.hb_font);
        }
    }
}

impl Shaper {
    pub(crate) fn new(font: &Font) -> Shaper {
        unsafe {
            let hb_face: *mut hb_face_t = hb_face_create_for_tables(
                Some(font_table_func),
                font as *const Font as *mut c_void,
                None,
            );
            let hb_font: *mut hb_font_t = hb_font_create(hb_face);

            // Set points-per-em. if zero, performs no hinting in that direction.
            let pt_size = font.descriptor.pt_size.to_f64_px();
            hb_font_set_ppem(hb_font, pt_size as c_uint, pt_size as c_uint);

            // Set scaling. Note that this takes 16.16 fixed point.
            hb_font_set_scale(
                hb_font,
                Shaper::float_to_fixed(pt_size) as c_int,
                Shaper::float_to_fixed(pt_size) as c_int,
            );

            if servo_config::pref!(layout_css_font_variations_enabled) {
                let variations = &font.variations();
                if !variations.is_empty() {
                    let variations: Vec<_> = variations
                        .iter()
                        .map(|variation| hb_variation_t {
                            tag: variation.tag,

                            value: variation.value,
                        })
                        .collect();

                    hb_font_set_variations(hb_font, variations.as_ptr(), variations.len() as u32);
                }
            }

            // Create a subfont before setting font-funcs so that we can
            // inherit the default font-funcs for the funcs we don't set.
            let hb_font = {
                let sub_font = hb_font_create_sub_font(hb_font);
                hb_font_destroy(hb_font);
                sub_font
            };

            // Now configure the subset of function callbacks that we do implement.
            hb_font_set_funcs(
                hb_font,
                HB_FONT_FUNCS.0,
                font as *const Font as *mut c_void,
                None,
            );

            Shaper { hb_face, hb_font }
        }
    }

    /// HarfBuzz returns these two percentage constants unscaled. Do not use
    /// fixed_to_float or the font's pixel size: 80 means a scale factor of 0.8.
    pub(crate) fn math_font_scales(&self) -> Option<MathFontScales> {
        unsafe {
            if hb_ot_math_has_data(self.hb_face) == 0 {
                return None;
            }
            let percentage = |constant| {
                let value = hb_ot_math_get_constant(self.hb_font, constant);
                (value > 0).then_some(value as f32 / 100.0)
            };
            Some(MathFontScales {
                script_percent_scale_down: percentage(
                    HB_OT_MATH_CONSTANT_SCRIPT_PERCENT_SCALE_DOWN,
                ),
                script_script_percent_scale_down: percentage(
                    HB_OT_MATH_CONSTANT_SCRIPT_SCRIPT_PERCENT_SCALE_DOWN,
                ),
            })
        }
    }

    pub(crate) fn math_constants(&self, is_display: bool) -> Option<MathConstants> {
        unsafe {
            if hb_ot_math_has_data(self.hb_face) == 0 {
                return None;
            }
            let get = |constant| {
                let pos = hb_ot_math_get_constant(self.hb_font, constant);
                Au::from_f64_px(Shaper::fixed_to_float(pos))
            };
            Some(MathConstants {
                axis_height: get(HB_OT_MATH_CONSTANT_AXIS_HEIGHT),
                display_operator_min_height: get(HB_OT_MATH_CONSTANT_DISPLAY_OPERATOR_MIN_HEIGHT),
                delimited_sub_formula_min_height: get(
                    HB_OT_MATH_CONSTANT_DELIMITED_SUB_FORMULA_MIN_HEIGHT,
                ),
                accent_base_height: get(HB_OT_MATH_CONSTANT_ACCENT_BASE_HEIGHT),
                flattened_accent_base_height: get(
                    HB_OT_MATH_CONSTANT_FLATTENED_ACCENT_BASE_HEIGHT,
                ),

                rule_thickness: get(HB_OT_MATH_CONSTANT_FRACTION_RULE_THICKNESS),
                num_shift_up: if is_display {
                    get(HB_OT_MATH_CONSTANT_FRACTION_NUMERATOR_DISPLAY_STYLE_SHIFT_UP)
                } else {
                    get(HB_OT_MATH_CONSTANT_FRACTION_NUMERATOR_SHIFT_UP)
                },
                num_gap_min: if is_display {
                    get(HB_OT_MATH_CONSTANT_FRACTION_NUM_DISPLAY_STYLE_GAP_MIN)
                } else {
                    get(HB_OT_MATH_CONSTANT_FRACTION_NUMERATOR_GAP_MIN)
                },
                denom_shift_down: if is_display {
                    get(HB_OT_MATH_CONSTANT_FRACTION_DENOMINATOR_DISPLAY_STYLE_SHIFT_DOWN)
                } else {
                    get(HB_OT_MATH_CONSTANT_FRACTION_DENOMINATOR_SHIFT_DOWN)
                },
                denom_gap_min: if is_display {
                    get(HB_OT_MATH_CONSTANT_FRACTION_DENOM_DISPLAY_STYLE_GAP_MIN)
                } else {
                    get(HB_OT_MATH_CONSTANT_FRACTION_DENOMINATOR_GAP_MIN)
                },
                stack_top_shift_up: if is_display {
                    get(HB_OT_MATH_CONSTANT_STACK_TOP_DISPLAY_STYLE_SHIFT_UP)
                } else {
                    get(HB_OT_MATH_CONSTANT_STACK_TOP_SHIFT_UP)
                },
                stack_bottom_shift_down: if is_display {
                    get(HB_OT_MATH_CONSTANT_STACK_BOTTOM_DISPLAY_STYLE_SHIFT_DOWN)
                } else {
                    get(HB_OT_MATH_CONSTANT_STACK_BOTTOM_SHIFT_DOWN)
                },
                stack_gap_min: if is_display {
                    get(HB_OT_MATH_CONSTANT_STACK_DISPLAY_STYLE_GAP_MIN)
                } else {
                    get(HB_OT_MATH_CONSTANT_STACK_GAP_MIN)
                },

                radical_vertical_gap: if is_display {
                    get(HB_OT_MATH_CONSTANT_RADICAL_DISPLAY_STYLE_VERTICAL_GAP)
                } else {
                    get(HB_OT_MATH_CONSTANT_RADICAL_VERTICAL_GAP)
                },
                radical_rule_thickness: get(HB_OT_MATH_CONSTANT_RADICAL_RULE_THICKNESS),
                radical_extra_ascender: get(HB_OT_MATH_CONSTANT_RADICAL_EXTRA_ASCENDER),
                radical_kern_before_degree: get(HB_OT_MATH_CONSTANT_RADICAL_KERN_BEFORE_DEGREE),
                radical_kern_after_degree: get(HB_OT_MATH_CONSTANT_RADICAL_KERN_AFTER_DEGREE),
                radical_degree_bottom_raise_percent: hb_ot_math_get_constant(
                    self.hb_font,
                    HB_OT_MATH_CONSTANT_RADICAL_DEGREE_BOTTOM_RAISE_PERCENT,
                ),

                subscript_shift_down: get(HB_OT_MATH_CONSTANT_SUBSCRIPT_SHIFT_DOWN),
                subscript_top_max: get(HB_OT_MATH_CONSTANT_SUBSCRIPT_TOP_MAX),
                subscript_baseline_drop_min: get(HB_OT_MATH_CONSTANT_SUBSCRIPT_BASELINE_DROP_MIN),
                superscript_shift_up: get(HB_OT_MATH_CONSTANT_SUPERSCRIPT_SHIFT_UP),
                superscript_shift_up_cramped: get(
                    HB_OT_MATH_CONSTANT_SUPERSCRIPT_SHIFT_UP_CRAMPED,
                ),
                superscript_bottom_min: get(HB_OT_MATH_CONSTANT_SUPERSCRIPT_BOTTOM_MIN),
                superscript_baseline_drop_max: get(
                    HB_OT_MATH_CONSTANT_SUPERSCRIPT_BASELINE_DROP_MAX,
                ),
                sub_superscript_gap_min: get(HB_OT_MATH_CONSTANT_SUB_SUPERSCRIPT_GAP_MIN),
                superscript_bottom_max_with_subscript: get(
                    HB_OT_MATH_CONSTANT_SUPERSCRIPT_BOTTOM_MAX_WITH_SUBSCRIPT,
                ),
                space_after_script: get(HB_OT_MATH_CONSTANT_SPACE_AFTER_SCRIPT),

                upper_limit_gap_min: get(HB_OT_MATH_CONSTANT_UPPER_LIMIT_GAP_MIN),
                upper_limit_baseline_rise_min: get(
                    HB_OT_MATH_CONSTANT_UPPER_LIMIT_BASELINE_RISE_MIN,
                ),
                lower_limit_gap_min: get(HB_OT_MATH_CONSTANT_LOWER_LIMIT_GAP_MIN),
                lower_limit_baseline_drop_min: get(
                    HB_OT_MATH_CONSTANT_LOWER_LIMIT_BASELINE_DROP_MIN,
                ),
                overbar_vertical_gap: get(HB_OT_MATH_CONSTANT_OVERBAR_VERTICAL_GAP),
                overbar_rule_thickness: get(HB_OT_MATH_CONSTANT_OVERBAR_RULE_THICKNESS),
                overbar_extra_ascender: get(HB_OT_MATH_CONSTANT_OVERBAR_EXTRA_ASCENDER),
                underbar_vertical_gap: get(HB_OT_MATH_CONSTANT_UNDERBAR_VERTICAL_GAP),
                underbar_rule_thickness: get(HB_OT_MATH_CONSTANT_UNDERBAR_RULE_THICKNESS),
                underbar_extra_descender: get(HB_OT_MATH_CONSTANT_UNDERBAR_EXTRA_DESCENDER),
                stretch_stack_top_shift_up: get(HB_OT_MATH_CONSTANT_STRETCH_STACK_TOP_SHIFT_UP),
                stretch_stack_bottom_shift_down: get(
                    HB_OT_MATH_CONSTANT_STRETCH_STACK_BOTTOM_SHIFT_DOWN,
                ),
                stretch_stack_gap_above_min: get(HB_OT_MATH_CONSTANT_STRETCH_STACK_GAP_ABOVE_MIN),
                stretch_stack_gap_below_min: get(HB_OT_MATH_CONSTANT_STRETCH_STACK_GAP_BELOW_MIN),
            })
        }
    }

    fn nominal_glyph(&self, ch: char) -> Option<GlyphId> {
        unsafe {
            let mut glyph: hb_codepoint_t = 0;
            if hb_font_get_nominal_glyph(self.hb_font, ch as hb_codepoint_t, &mut glyph) != 0 {
                Some(glyph)
            } else {
                None
            }
        }
    }

    fn glyph_extents_by_id(&self, glyph: GlyphId) -> MathGlyphExtents {
        unsafe {
            let advance = Au::from_f64_px(Shaper::fixed_to_float(hb_font_get_glyph_h_advance(
                self.hb_font,
                glyph,
            )));
            let mut extents = hb_glyph_extents_t {
                x_bearing: 0,
                y_bearing: 0,
                width: 0,
                height: 0,
            };
            if hb_font_get_glyph_extents(self.hb_font, glyph, &mut extents) != 0 {
                let ascent = Au::from_f64_px(Shaper::fixed_to_float(extents.y_bearing));
                let descent =
                    Au::from_f64_px(Shaper::fixed_to_float(-(extents.y_bearing + extents.height)));
                let x_bearing = Au::from_f64_px(Shaper::fixed_to_float(extents.x_bearing));
                let width = Au::from_f64_px(Shaper::fixed_to_float(extents.width));
                MathGlyphExtents {
                    advance,
                    ascent,
                    descent,
                    x_bearing,
                    width,
                }
            } else {
                MathGlyphExtents {
                    advance,
                    ascent: advance,
                    descent: Au::zero(),
                    x_bearing: Au::zero(),
                    width: advance,
                }
            }
        }
    }

    pub(crate) fn math_glyph_extents(&self, ch: char) -> Option<MathGlyphExtents> {
        let glyph = self.nominal_glyph(ch)?;
        Some(self.glyph_extents_by_id(glyph))
    }

    pub(crate) fn math_char_italics_correction(&self, ch: char) -> Au {
        unsafe {
            if hb_ot_math_has_data(self.hb_face) == 0 {
                return Au::zero();
            }
            let Some(glyph) = self.nominal_glyph(ch) else {
                return Au::zero();
            };
            let ic = hb_ot_math_get_glyph_italics_correction(self.hb_font, glyph);
            Au::from_f64_px(Shaper::fixed_to_float(ic))
        }
    }

    pub(crate) fn math_char_top_accent_attachment(&self, ch: char) -> Option<Au> {
        unsafe {
            if hb_ot_math_has_data(self.hb_face) == 0 {
                return None;
            }
            let glyph = self.nominal_glyph(ch)?;
            let pos = hb_ot_math_get_glyph_top_accent_attachment(self.hb_font, glyph);
            Some(Au::from_f64_px(Shaper::fixed_to_float(pos)))
        }
    }

    /// Stretch `ch` along `direction` to cover at least `target_size` using the
    /// OpenType MATH table's size variants or multi-part glyph assembly.
    /// See <https://w3c.github.io/mathml-core/#algorithms-for-glyph-stretching>.
    pub(crate) fn math_stretch_glyph(
        &self,
        ch: char,
        direction: MathStretchDirection,
        target_size: Au,
    ) -> Option<StretchedMathGlyph> {
        let base_glyph = self.nominal_glyph(ch)?;
        let hb_dir = match direction {
            MathStretchDirection::Horizontal => HB_DIRECTION_LTR,
            MathStretchDirection::Vertical => HB_DIRECTION_TTB,
        };

        let make_single = |glyph_id: GlyphId| -> StretchedMathGlyph {
            let ext = self.glyph_extents_by_id(glyph_id);
            let (ic, accent) = unsafe {
                if hb_ot_math_has_data(self.hb_face) != 0 {
                    let ic = Au::from_f64_px(Shaper::fixed_to_float(
                        hb_ot_math_get_glyph_italics_correction(self.hb_font, glyph_id),
                    ));
                    let accent = Au::from_f64_px(Shaper::fixed_to_float(
                        hb_ot_math_get_glyph_top_accent_attachment(self.hb_font, glyph_id),
                    ));
                    (ic, Some(accent))
                } else {
                    (Au::zero(), None)
                }
            };
            StretchedMathGlyph {
                glyphs: vec![PositionedMathGlyph {
                    glyph_id,
                    x_offset: Au::zero(),
                    y_offset: Au::zero(),
                }],
                inline_size: ext.advance.max(Au::zero()),
                ascent: ext.ascent,
                descent: ext.descent,
                italics_correction: ic,
                top_accent_attachment: accent,
            }
        };

        unsafe {
            if hb_ot_math_has_data(self.hb_face) == 0 {
                return Some(make_single(base_glyph));
            }

            // 1. Query size variants.
            let mut variant_count: c_uint = 0;
            let total_variants = hb_ot_math_get_glyph_variants(
                self.hb_font,
                base_glyph,
                hb_dir,
                0,
                &mut variant_count,
                ptr::null_mut(),
            );
            let mut variants = Vec::new();
            if total_variants > 0 {
                variants.resize(
                    total_variants as usize,
                    hb_ot_math_glyph_variant_t {
                        glyph: 0,
                        advance: 0,
                    },
                );
                let mut count = total_variants;
                hb_ot_math_get_glyph_variants(
                    self.hb_font,
                    base_glyph,
                    hb_dir,
                    0,
                    &mut count,
                    variants.as_mut_ptr(),
                );
                variants.truncate(count as usize);

                for variant in &variants {
                    let advance = Au::from_f64_px(Shaper::fixed_to_float(variant.advance));
                    if advance >= target_size {
                        return Some(make_single(variant.glyph));
                    }
                }
            }

            // 2. Query multi-part glyph assembly if variants were not large enough.
            let mut part_count: c_uint = 0;
            let mut assembly_ic: hb_position_t = 0;
            let total_parts = hb_ot_math_get_glyph_assembly(
                self.hb_font,
                base_glyph,
                hb_dir,
                0,
                &mut part_count,
                ptr::null_mut(),
                &mut assembly_ic,
            );
            if total_parts > 0 {
                let mut parts = vec![
                    hb_ot_math_glyph_part_t {
                        glyph: 0,
                        start_connector_length: 0,
                        end_connector_length: 0,
                        full_advance: 0,
                        flags: 0,
                    };
                    total_parts as usize
                ];
                let mut count = total_parts;
                hb_ot_math_get_glyph_assembly(
                    self.hb_font,
                    base_glyph,
                    hb_dir,
                    0,
                    &mut count,
                    parts.as_mut_ptr(),
                    &mut assembly_ic,
                );
                parts.truncate(count as usize);

                if !parts.is_empty() {
                    let min_overlap = Au::from_f64_px(Shaper::fixed_to_float(
                        hb_ot_math_get_min_connector_overlap(self.hb_font, hb_dir),
                    ))
                    .max(Au::zero());
                    let ic = Au::from_f64_px(Shaper::fixed_to_float(assembly_ic));
                    if let Some(assembled) =
                        self.assemble_math_glyph(&parts, direction, target_size, min_overlap, ic)
                    {
                        return Some(assembled);
                    }
                }
            }

            // 3. Fallback: largest available variant, or base glyph.
            if let Some(last) = variants.last() {
                return Some(make_single(last.glyph));
            }
            Some(make_single(base_glyph))
        }
    }

    fn assemble_math_glyph(
        &self,
        parts: &[hb_ot_math_glyph_part_t],
        direction: MathStretchDirection,
        target_size: Au,
        min_overlap: Au,
        italics_correction: Au,
    ) -> Option<StretchedMathGlyph> {
        let is_extender =
            |p: &hb_ot_math_glyph_part_t| (p.flags & HB_OT_MATH_GLYPH_PART_FLAG_EXTENDER) != 0;
        let to_au = |pos: hb_position_t| Au::from_f64_px(Shaper::fixed_to_float(pos));

        let non_extender_count = parts.iter().filter(|p| !is_extender(p)).count();
        let extender_count = parts.len() - non_extender_count;

        // Build part sequence for a given extender repetition count `k`.
        let build_sequence = |k: usize| -> Vec<&hb_ot_math_glyph_part_t> {
            let mut seq = Vec::with_capacity(non_extender_count + extender_count * k);
            for part in parts {
                if is_extender(part) {
                    for _ in 0..k {
                        seq.push(part);
                    }
                } else {
                    seq.push(part);
                }
            }
            seq
        };

        // Compute maximum achievable span (using min_overlap) for a sequence.
        let max_span_for = |seq: &[&hb_ot_math_glyph_part_t]| -> Au {
            if seq.is_empty() {
                return Au::zero();
            }
            let mut span = to_au(seq[0].full_advance);
            for pair in seq.windows(2) {
                let max_conn = to_au(pair[0].end_connector_length)
                    .min(to_au(pair[1].start_connector_length));
                let overlap = min_overlap.min(max_conn).max(Au::zero());
                span += to_au(pair[1].full_advance) - overlap;
            }
            span
        };

        // Find smallest k >= 0 (or k >= 1 if there are no non-extenders) that reaches target_size.
        let mut k = if non_extender_count == 0 { 1 } else { 0 };
        let max_k = 256usize;
        let mut seq = build_sequence(k);
        if extender_count > 0 {
            while max_span_for(&seq) < target_size && k < max_k {
                k += 1;
                seq = build_sequence(k);
            }
        }
        if seq.is_empty() {
            return None;
        }

        // Compute connector overlaps, distributing any excess above target_size smoothly.
        let num_joints = seq.len().saturating_sub(1);
        let mut overlaps = Vec::with_capacity(num_joints);
        let mut max_extra_overlap = Au::zero();
        for pair in seq.windows(2) {
            let max_conn = to_au(pair[0].end_connector_length)
                .min(to_au(pair[1].start_connector_length))
                .max(Au::zero());
            let base_ov = min_overlap.min(max_conn);
            overlaps.push((base_ov, max_conn));
            max_extra_overlap += max_conn - base_ov;
        }

        let current_span = max_span_for(&seq);
        let excess = (current_span - target_size).max(Au::zero());
        let extra_ratio = if max_extra_overlap > Au::zero() && excess > Au::zero() {
            (excess.0 as f64 / max_extra_overlap.0 as f64).min(1.0)
        } else {
            0.0
        };

        let mut positioned = Vec::with_capacity(seq.len());
        let mut cursor = Au::zero();

        match direction {
            MathStretchDirection::Horizontal => {
                let mut max_ascent = Au::zero();
                let mut max_descent = Au::zero();
                for (i, part) in seq.iter().enumerate() {
                    if i > 0 {
                        let (base_ov, max_conn) = overlaps[i - 1];
                        let extra = Au(((max_conn - base_ov).0 as f64 * extra_ratio) as i32);
                        cursor -= base_ov + extra;
                    }
                    let ext = self.glyph_extents_by_id(part.glyph);
                    max_ascent = max_ascent.max(ext.ascent);
                    max_descent = max_descent.max(ext.descent);
                    positioned.push(PositionedMathGlyph {
                        glyph_id: part.glyph,
                        x_offset: cursor,
                        y_offset: Au::zero(),
                    });
                    cursor += to_au(part.full_advance);
                }
                Some(StretchedMathGlyph {
                    glyphs: positioned,
                    inline_size: cursor.max(Au::zero()),
                    ascent: max_ascent,
                    descent: max_descent,
                    italics_correction,
                    top_accent_attachment: None,
                })
            },
            MathStretchDirection::Vertical => {
                // Vertical assemblies in OpenType MATH are ordered bottom-to-top.
                // We place part 0 at y = 0 and step upward (negative y_offset in CSS y-down).
                let mut max_inline = Au::zero();
                let mut max_ascent = Au::zero();
                let mut max_descent = Au::zero();
                for (i, part) in seq.iter().enumerate() {
                    if i > 0 {
                        let (base_ov, max_conn) = overlaps[i - 1];
                        let extra = Au(((max_conn - base_ov).0 as f64 * extra_ratio) as i32);
                        cursor -= base_ov + extra;
                    }
                    let ext = self.glyph_extents_by_id(part.glyph);
                    max_inline = max_inline.max(ext.advance);
                    max_ascent = max_ascent.max(cursor + ext.ascent);
                    max_descent = max_descent.max(ext.descent - cursor);
                    positioned.push(PositionedMathGlyph {
                        glyph_id: part.glyph,
                        x_offset: Au::zero(),
                        y_offset: -cursor,
                    });
                    cursor += to_au(part.full_advance);
                }
                Some(StretchedMathGlyph {
                    glyphs: positioned,
                    inline_size: max_inline.max(Au::zero()),
                    ascent: max_ascent,
                    descent: max_descent,
                    italics_correction,
                    top_accent_attachment: None,
                })
            },
        }
    }

    /// Calculate the layout metrics associated with the given text with the [`Shaper`]s font.
    pub(crate) fn shaped_glyph_data(
        &self,
        _: &Font,
        text: &str,
        options: &ShapingOptions,
        font_features: &[(Tag, u32)],
    ) -> HarfbuzzGlyphShapingResult {
        unsafe {
            let hb_buffer: *mut hb_buffer_t = hb_buffer_create();
            hb_buffer_set_cluster_level(hb_buffer, HB_BUFFER_CLUSTER_LEVEL_MONOTONE_CHARACTERS);
            hb_buffer_set_direction(
                hb_buffer,
                if options.flags.contains(ShapingFlags::RTL_FLAG) {
                    HB_DIRECTION_RTL
                } else {
                    HB_DIRECTION_LTR
                },
            );

            let script =
                hb_script_from_iso15924_tag(unicode_script_to_iso15924_tag(options.script));
            hb_buffer_set_script(hb_buffer, script);

            hb_buffer_add_utf8(
                hb_buffer,
                text.as_ptr() as *const c_char,
                text.len() as c_int,
                0,
                text.len() as c_int,
            );

            let hb_language = if let Some(language_override) = options.language_override {
                hb_ot_tag_to_language(u32::from_be_bytes(language_override.to_be_bytes()))
            } else if !options.language.is_empty() {
                let language: &str = &options.language;
                hb_language_from_string(language.as_ptr() as *const c_char, language.len() as c_int)
            } else {
                hb_ot_tag_to_language(HB_OT_TAG_DEFAULT_LANGUAGE)
            };
            hb_buffer_set_language(hb_buffer, hb_language);

            let mut features: Vec<_> = font_features
                .iter()
                .map(|(tag, value)| hb_feature_t {
                    tag: u32::from_be_bytes(tag.to_be_bytes()),
                    value: *value,
                    start: 0,      // HB_FEATURE_GLOBAL_START
                    end: u32::MAX, // HB_FEATURE_GLOBAL_END
                })
                .collect();
            hb_shape(
                self.hb_font,
                hb_buffer,
                features.as_mut_ptr(),
                features.len() as u32,
            );

            HarfbuzzGlyphShapingResult::new(hb_buffer)
        }
    }

    #[allow(dead_code)]
    pub(crate) fn shape_text(
        &self,
        font: &Font,
        text: &str,
        options: &ShapingOptions,
        font_features: &[(Tag, u32)],
    ) -> ShapedText {
        ShapedText::with_shaped_glyph_data(
            text,
            options,
            &self.shaped_glyph_data(font, text, options, font_features),
        )
    }

    fn float_to_fixed(f: f64) -> i32 {
        float_to_fixed(16, f)
    }

    fn fixed_to_float(i: hb_position_t) -> f64 {
        fixed_to_float(16, i)
    }
}

/// Callbacks from Harfbuzz when font map and glyph advance lookup needed.
struct FontFuncs(*mut hb_font_funcs_t);

unsafe impl Sync for FontFuncs {}
unsafe impl Send for FontFuncs {}

static HB_FONT_FUNCS: LazyLock<FontFuncs> = LazyLock::new(|| unsafe {
    let hb_funcs = hb_font_funcs_create();
    hb_font_funcs_set_nominal_glyph_func(hb_funcs, Some(glyph_func), ptr::null_mut(), None);
    hb_font_funcs_set_glyph_h_advance_func(
        hb_funcs,
        Some(glyph_h_advance_func),
        ptr::null_mut(),
        None,
    );

    FontFuncs(hb_funcs)
});

extern "C" fn glyph_func(
    _: *mut hb_font_t,
    font_data: *mut c_void,
    unicode: hb_codepoint_t,
    glyph: *mut hb_codepoint_t,
    _: *mut c_void,
) -> hb_bool_t {
    let font: *const Font = font_data as *const Font;
    assert!(!font.is_null());

    match unsafe { (*font).glyph_index(char::from_u32(unicode).unwrap()) } {
        Some(g) => {
            unsafe { *glyph = g as hb_codepoint_t };
            true as hb_bool_t
        },
        None => false as hb_bool_t,
    }
}

extern "C" fn glyph_h_advance_func(
    _: *mut hb_font_t,
    font_data: *mut c_void,
    glyph: hb_codepoint_t,
    _: *mut c_void,
) -> hb_position_t {
    let font: *mut Font = font_data as *mut Font;
    assert!(!font.is_null());

    let advance = unsafe { (*font).glyph_h_advance(glyph as GlyphId) };
    Shaper::float_to_fixed(advance)
}

/// Callback to get a font table out of a font.
extern "C" fn font_table_func(
    _: *mut hb_face_t,
    tag: hb_tag_t,
    user_data: *mut c_void,
) -> *mut hb_blob_t {
    // NB: These asserts have security implications.
    let font = user_data as *const Font;
    assert!(!font.is_null());

    // TODO(Issue #197): reuse font table data, which will change the unsound trickery here.
    let Some(font_table) = (unsafe { (*font).table_for_tag(Tag::from_u32(tag)) }) else {
        return ptr::null_mut();
    };

    // `Box::into_raw` intentionally leaks the FontTable so we don't destroy the buffer
    // while HarfBuzz is using it.  When HarfBuzz is done with the buffer, it will pass
    // this raw pointer back to `destroy_blob_func` which will deallocate the Box.
    let font_table_ptr = Box::into_raw(Box::new(font_table));

    let buf = unsafe { (*font_table_ptr).buffer() };
    // HarfBuzz calls `destroy_blob_func` when the buffer is no longer needed.
    let blob = unsafe {
        hb_blob_create(
            buf.as_ptr() as *const c_char,
            buf.len() as c_uint,
            HB_MEMORY_MODE_READONLY,
            font_table_ptr as *mut c_void,
            Some(destroy_blob_func),
        )
    };

    assert!(!blob.is_null());
    blob
}

extern "C" fn destroy_blob_func(font_table_ptr: *mut c_void) {
    unsafe {
        drop(Box::from_raw(font_table_ptr as *mut FontTable));
    }
}
